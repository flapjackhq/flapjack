//! Bounded source identity partitioning contract.
#![allow(dead_code)]

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    mem,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use super::source_snapshot::{
    object_stable_id, source_item_hash, update_source_item_hash_digest,
    SourceSnapshotSchemaViolationKind,
};

const IDENTITY_BUDGET_ENV: &str = "FLAPJACK_MIGRATION_IDENTITY_BUDGET_BYTES";
const IDENTITY_DIGEST_HEX_BYTES: usize = 64;
pub(super) const IDENTITY_TUPLE_BYTES: usize = 128; // Planning estimate only; runtime bound is enforced in real bytes.
pub(super) const PARTITION_SKEW_HEADROOM: u64 = 4; // Canonical partition skew safety factor.
pub(super) const DEFAULT_IDENTITY_BUDGET_BYTES: usize = 16 * 1024 * 1024; // Default migration identity memory budget.
pub(super) const CERTIFIED_MAX_ITEMS: u64 = 64_000_000; // Canonical plan's certification target.
pub(super) const IDENTITY_V2_DOMAIN: &[u8] = b"flapjack-source-identity-v2\n"; // Digest preimage domain separator.

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SourceIdentityConfig {
    pub(super) spool_root: PathBuf,
    pub(super) budget_bytes: usize,
    pub(super) certified_max_items: u64,
    spool_root_ownership: SpoolRootOwnership,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpoolRootOwnership {
    CallerProvided,
    Implicit,
}

impl SourceIdentityConfig {
    pub(super) fn from_env() -> Result<Self, SourceIdentityError> {
        let (spool_root, spool_root_ownership) =
            match std::env::var("FLAPJACK_MIGRATION_IDENTITY_SPOOL_DIR") {
                Ok(path) => (PathBuf::from(path), SpoolRootOwnership::CallerProvided),
                Err(std::env::VarError::NotPresent) => (
                    std::env::temp_dir()
                        .join(format!("flapjack-migration-identity-{}", Uuid::new_v4())),
                    SpoolRootOwnership::Implicit,
                ),
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(SourceIdentityError::InvalidConfig {
                        name: "FLAPJACK_MIGRATION_IDENTITY_SPOOL_DIR",
                    });
                }
            };
        let budget_bytes = match std::env::var(IDENTITY_BUDGET_ENV) {
            Ok(value) => parse_budget_env(&value)?,
            Err(std::env::VarError::NotPresent) => DEFAULT_IDENTITY_BUDGET_BYTES,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(SourceIdentityError::InvalidConfig {
                    name: IDENTITY_BUDGET_ENV,
                });
            }
        };
        let config = Self {
            spool_root,
            budget_bytes,
            certified_max_items: CERTIFIED_MAX_ITEMS,
            spool_root_ownership,
        };
        config.validate()?;
        Ok(config)
    }

    pub(super) fn for_test(
        spool_root: &Path,
        budget_bytes: usize,
        certified_max_items: u64,
    ) -> Self {
        Self {
            spool_root: spool_root.to_path_buf(),
            budget_bytes,
            certified_max_items,
            spool_root_ownership: SpoolRootOwnership::CallerProvided,
        }
    }

    pub(super) fn max_resident_bytes(&self) -> usize {
        self.budget_bytes
    }

    pub(super) fn validate(&self) -> Result<(), SourceIdentityError> {
        let fixed_resident_bytes =
            partition_buffer_bytes(self.partition_count()).max(IDENTITY_DIGEST_HEX_BYTES);
        if fixed_resident_bytes > self.budget_bytes {
            return Err(SourceIdentityError::InvalidConfig {
                name: IDENTITY_BUDGET_ENV,
            });
        }
        Ok(())
    }

    // partition_count raw = ceil(certified_max_items * PARTITION_SKEW_HEADROOM
    // / (budget_bytes / IDENTITY_TUPLE_BYTES)), rounded up to the next power of two, minimum 1.
    // Defaults give 16 MiB/128 = 131_072 -> 64_000_000*4/131_072 = 1953.125 -> 1954
    // -> next_power_of_two = 2048.
    pub(super) fn partition_count(&self) -> u32 {
        let tuples_per_partition = (self.budget_bytes / IDENTITY_TUPLE_BYTES).max(1) as u64;
        let required = self
            .certified_max_items
            .saturating_mul(PARTITION_SKEW_HEADROOM)
            .div_ceil(tuples_per_partition)
            .max(1);
        required.next_power_of_two().min(u32::MAX as u64) as u32
    }
}

#[derive(Debug)]
pub(super) struct SourceIdentityPartitions {
    spool_dir: PathBuf,
    budget_bytes: usize,
    partition_count: u32,
    buffers: Box<[Option<Box<BufferedTuple>>]>,
    count: usize,
    resident_bytes: usize,
    resident_tuples: usize,
    max_resident_bytes_observed: usize,
    max_resident_tuples_observed: usize,
    #[cfg(test)]
    tuple_allocations: usize,
}

impl SourceIdentityPartitions {
    pub(super) fn new(config: SourceIdentityConfig) -> Result<Self, SourceIdentityError> {
        config.validate()?;
        let partition_count = config.partition_count();
        let SourceIdentityConfig {
            mut spool_root,
            budget_bytes,
            certified_max_items: _,
            spool_root_ownership,
        } = config;
        let spool_dir = match spool_root_ownership {
            SpoolRootOwnership::CallerProvided => {
                spool_root.push(format!("identity-partitions-{}", Uuid::new_v4()));
                spool_root
            }
            SpoolRootOwnership::Implicit => spool_root,
        };
        fs::create_dir_all(&spool_dir).map_err(SourceIdentityError::Io)?;
        let buffers = std::iter::repeat_with(|| None)
            .take(partition_count as usize)
            .collect::<Box<[_]>>();
        let resident_bytes = partition_buffer_bytes(partition_count);
        Ok(Self {
            spool_dir,
            budget_bytes,
            partition_count,
            buffers,
            count: 0,
            resident_bytes,
            resident_tuples: 0,
            max_resident_bytes_observed: resident_bytes,
            max_resident_tuples_observed: 0,
            #[cfg(test)]
            tuple_allocations: 0,
        })
    }

    pub(super) fn partition_count(&self) -> u32 {
        self.partition_count
    }

    pub(super) fn record(
        &mut self,
        object_id: &str,
        item_hash: &str,
        page_index: usize,
        item_index: usize,
    ) -> Result<(), SourceIdentityError> {
        let partition = self.partition_for(object_id);
        let tuple_bytes = buffered_tuple_bytes(object_id, item_hash);
        if self.resident_bytes.saturating_add(tuple_bytes) > self.budget_bytes {
            self.flush_all()?;
        }
        let bytes = self.resident_bytes.saturating_add(tuple_bytes);
        if bytes > self.budget_bytes {
            return Err(SourceIdentityError::PartitionBudgetExceeded {
                partition,
                bytes,
                budget_bytes: self.budget_bytes,
            });
        }
        let encoded = encode_tuple(object_id, item_hash, page_index, item_index)?;
        let next = self.buffers[partition as usize].take();
        self.buffers[partition as usize] = Some(Box::new(BufferedTuple { encoded, next }));
        #[cfg(test)]
        {
            self.tuple_allocations += 1;
        }
        self.count += 1;
        self.resident_bytes += tuple_bytes;
        self.resident_tuples += 1;
        self.update_high_water(self.resident_bytes, self.resident_tuples);
        Ok(())
    }

    pub(super) fn record_item(
        &mut self,
        item: &Value,
        page_index: usize,
        item_index: usize,
    ) -> Result<(), SourceIdentityError> {
        let object_id = object_stable_id(item).map_err(|kind| match kind {
            SourceSnapshotSchemaViolationKind::MalformedPayload => {
                SourceIdentityError::MalformedPayload {
                    page_index,
                    item_index,
                }
            }
            SourceSnapshotSchemaViolationKind::InvalidObjectId
            | SourceSnapshotSchemaViolationKind::DuplicateObjectId => {
                SourceIdentityError::InvalidObjectId {
                    page_index,
                    item_index,
                }
            }
        })?;
        let item_hash = source_item_hash(item);
        self.record(&object_id, &item_hash, page_index, item_index)
    }

    pub(super) fn finish(mut self) -> Result<SourceIdentityOutcome, SourceIdentityError> {
        self.flush_all()?;
        self.release_partition_buffers();
        let mut identity_hasher = identity_v2_hasher(self.partition_count);
        for partition in 0..self.partition_count {
            let path = self.partition_path(partition);
            if !path.exists() {
                continue;
            }
            let partition_load = read_partition_tuples(&path, partition, self.budget_bytes)?;
            self.update_high_water(
                partition_load.resident_bytes,
                partition_load.tuple_offsets.len(),
            );
            let mut partition_load = partition_load;
            partition_load.sort();
            if let Some((first, second)) = partition_load.first_duplicate() {
                return Err(SourceIdentityError::Duplicate { first, second });
            }
            update_identity_v2_digest(&mut identity_hasher, partition, partition_load.digest());
        }
        let digest = hex::encode(identity_hasher.finalize());
        self.update_high_water(digest.capacity(), 0);
        Ok(SourceIdentityOutcome {
            digest,
            count: self.count,
            partition_count: self.partition_count,
            version: SourceIdentityVersion::V2,
            max_resident_bytes_observed: self.max_resident_bytes_observed,
            max_resident_tuples_observed: self.max_resident_tuples_observed,
        })
    }

    fn flush_all(&mut self) -> Result<(), SourceIdentityError> {
        for partition in 0..self.partition_count {
            self.flush_partition(partition)?;
        }
        Ok(())
    }

    fn flush_partition(&mut self, partition: u32) -> Result<(), SourceIdentityError> {
        let mut buffer = self.buffers[partition as usize].take();
        if buffer.is_none() {
            return Ok(());
        }
        let mut file = append_partition_file(&self.partition_path(partition))?;
        while let Some(mut tuple) = buffer {
            file.write_all(&tuple.encoded)
                .map_err(SourceIdentityError::Io)?;
            self.resident_bytes -= mem::size_of::<BufferedTuple>() + tuple.encoded.len();
            self.resident_tuples -= 1;
            buffer = tuple.next.take();
        }
        Ok(())
    }

    fn release_partition_buffers(&mut self) {
        self.buffers = Box::new([]);
        self.resident_bytes = 0;
    }

    fn partition_for(&self, object_id: &str) -> u32 {
        let digest = Sha256::digest(object_id.as_bytes());
        let first_eight = digest[..8]
            .try_into()
            .expect("sha256 digest always contains at least 8 bytes");
        (u64::from_be_bytes(first_eight) % u64::from(self.partition_count)) as u32
    }

    fn partition_path(&self, partition: u32) -> PathBuf {
        self.spool_dir.join(format!("partition_{partition}"))
    }

    #[cfg(test)]
    pub(super) fn partition_path_for_test(&self, partition: u32) -> PathBuf {
        self.partition_path(partition)
    }

    #[cfg(test)]
    pub(super) fn tuple_allocations_for_test(&self) -> usize {
        self.tuple_allocations
    }

    fn update_high_water(&mut self, bytes: usize, tuples: usize) {
        self.max_resident_bytes_observed = self.max_resident_bytes_observed.max(bytes);
        self.max_resident_tuples_observed = self.max_resident_tuples_observed.max(tuples);
    }
}

impl Drop for SourceIdentityPartitions {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.spool_dir);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceIdentityOutcome {
    pub(super) digest: String,
    pub(super) count: usize,
    pub(super) partition_count: u32,
    pub(super) version: SourceIdentityVersion,
    max_resident_bytes_observed: usize,
    max_resident_tuples_observed: usize,
}

impl SourceIdentityOutcome {
    pub(super) fn max_resident_bytes_observed(&self) -> usize {
        self.max_resident_bytes_observed
    }

    pub(super) fn max_resident_tuples_observed(&self) -> usize {
        self.max_resident_tuples_observed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceIdentityVersion {
    V1,
    V2,
}

#[derive(Debug)]
pub(super) enum SourceIdentityError {
    Duplicate {
        first: (usize, usize),
        second: (usize, usize),
    },
    InvalidObjectId {
        page_index: usize,
        item_index: usize,
    },
    MalformedPayload {
        page_index: usize,
        item_index: usize,
    },
    PartitionBudgetExceeded {
        partition: u32,
        bytes: usize,
        budget_bytes: usize,
    },
    InvalidConfig {
        name: &'static str,
    },
    Io(io::Error),
}

impl SourceIdentityError {
    pub(super) fn safe_message(&self) -> &'static str {
        match self {
            Self::Duplicate { .. } => "duplicate source objectID",
            Self::InvalidObjectId { .. } => "source item was missing a string objectID",
            Self::MalformedPayload { .. } => "source item was not a JSON object",
            Self::PartitionBudgetExceeded { .. } => {
                "source identity partition exceeded memory budget"
            }
            Self::InvalidConfig { .. } => "source identity configuration was invalid",
            Self::Io(_) => "source identity partition I/O failed",
        }
    }

    pub(super) fn is_infrastructure(&self) -> bool {
        matches!(
            self,
            Self::PartitionBudgetExceeded { .. } | Self::InvalidConfig { .. } | Self::Io(_)
        )
    }
}

impl PartialEq for SourceIdentityError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Duplicate {
                    first: left_first,
                    second: left_second,
                },
                Self::Duplicate {
                    first: right_first,
                    second: right_second,
                },
            ) => left_first == right_first && left_second == right_second,
            (
                Self::InvalidObjectId {
                    page_index: left_page,
                    item_index: left_item,
                },
                Self::InvalidObjectId {
                    page_index: right_page,
                    item_index: right_item,
                },
            ) => left_page == right_page && left_item == right_item,
            (
                Self::MalformedPayload {
                    page_index: left_page,
                    item_index: left_item,
                },
                Self::MalformedPayload {
                    page_index: right_page,
                    item_index: right_item,
                },
            ) => left_page == right_page && left_item == right_item,
            (
                Self::PartitionBudgetExceeded {
                    partition: left_partition,
                    bytes: left_bytes,
                    budget_bytes: left_budget,
                },
                Self::PartitionBudgetExceeded {
                    partition: right_partition,
                    bytes: right_bytes,
                    budget_bytes: right_budget,
                },
            ) => {
                left_partition == right_partition
                    && left_bytes == right_bytes
                    && left_budget == right_budget
            }
            (Self::InvalidConfig { name: left }, Self::InvalidConfig { name: right }) => {
                left == right
            }
            (Self::Io(left), Self::Io(right)) => left.kind() == right.kind(),
            _ => false,
        }
    }
}

impl Eq for SourceIdentityError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceIdentityReceipt {
    pub(super) version: SourceIdentityVersion,
    pub(super) digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum IdentityComparisonError {
    VersionMismatch {
        receipt: SourceIdentityVersion,
        current: SourceIdentityVersion,
    },
    DigestMismatch,
}

pub(super) fn compare_receipt(
    receipt: &SourceIdentityReceipt,
    current: &SourceIdentityOutcome,
) -> Result<(), IdentityComparisonError> {
    if receipt.version != current.version {
        return Err(IdentityComparisonError::VersionMismatch {
            receipt: receipt.version,
            current: current.version,
        });
    }
    if receipt.digest != current.digest {
        return Err(IdentityComparisonError::DigestMismatch);
    }
    Ok(())
}

#[derive(Debug)]
struct BufferedTuple {
    encoded: Box<[u8]>,
    next: Option<Box<BufferedTuple>>,
}

#[derive(Clone, Copy, Default)]
struct PartitionTupleOffset {
    object_id_start: usize,
    object_id_length: usize,
    item_hash_start: usize,
    item_hash_length: usize,
    page_index: usize,
    item_index: usize,
}

impl PartitionTupleOffset {
    fn object_id<'a>(&self, encoded: &'a [u8]) -> &'a str {
        validated_spool_string(encoded, self.object_id_start, self.object_id_length)
    }

    fn item_hash<'a>(&self, encoded: &'a [u8]) -> &'a str {
        validated_spool_string(encoded, self.item_hash_start, self.item_hash_length)
    }
}

struct PartitionLoad {
    encoded: Box<[u8]>,
    tuple_offsets: Box<[PartitionTupleOffset]>,
    resident_bytes: usize,
}

impl PartitionLoad {
    fn sort(&mut self) {
        let encoded = &self.encoded;
        self.tuple_offsets
            .sort_unstable_by(|left, right| compare_tuples(encoded, left, right));
    }

    fn first_duplicate(&self) -> Option<((usize, usize), (usize, usize))> {
        self.tuple_offsets.windows(2).find_map(|pair| {
            let first = pair[0];
            let second = pair[1];
            (first.object_id(&self.encoded) == second.object_id(&self.encoded)).then_some((
                (first.page_index, first.item_index),
                (second.page_index, second.item_index),
            ))
        })
    }

    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for tuple in &self.tuple_offsets {
            update_source_item_hash_digest(
                &mut hasher,
                tuple.object_id(&self.encoded),
                tuple.item_hash(&self.encoded),
            );
        }
        hasher.finalize().into()
    }
}

struct PartitionCursor<'a> {
    encoded: &'a [u8],
    position: usize,
}

impl<'a> PartitionCursor<'a> {
    fn next_tuple(&mut self) -> Result<Option<PartitionTupleOffset>, SourceIdentityError> {
        if self.position == self.encoded.len() {
            return Ok(None);
        }
        let (object_id_start, object_id_length) = self.read_string_range()?;
        let (item_hash_start, item_hash_length) = self.read_string_range()?;
        let page_index = self.read_usize()?;
        let item_index = self.read_usize()?;
        Ok(Some(PartitionTupleOffset {
            object_id_start,
            object_id_length,
            item_hash_start,
            item_hash_length,
            page_index,
            item_index,
        }))
    }

    fn read_string_range(&mut self) -> Result<(usize, usize), SourceIdentityError> {
        let length = self.read_usize()?;
        let start = self.position;
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.encoded.len())
            .ok_or_else(corrupt_partition_error)?;
        std::str::from_utf8(&self.encoded[start..end]).map_err(|_| corrupt_partition_error())?;
        self.position = end;
        Ok((start, length))
    }

    fn read_usize(&mut self) -> Result<usize, SourceIdentityError> {
        let end = self
            .position
            .checked_add(mem::size_of::<u64>())
            .filter(|end| *end <= self.encoded.len())
            .ok_or_else(corrupt_partition_error)?;
        let bytes = self.encoded[self.position..end]
            .try_into()
            .expect("u64 spool field has a fixed width");
        self.position = end;
        usize::try_from(u64::from_be_bytes(bytes)).map_err(|_| corrupt_partition_error())
    }
}

fn parse_budget_env(value: &str) -> Result<usize, SourceIdentityError> {
    match value.parse::<usize>() {
        Ok(0) | Err(_) => Err(SourceIdentityError::InvalidConfig {
            name: IDENTITY_BUDGET_ENV,
        }),
        Ok(budget_bytes) => Ok(budget_bytes),
    }
}

fn partition_buffer_bytes(partition_count: u32) -> usize {
    (partition_count as usize).saturating_mul(mem::size_of::<Option<Box<BufferedTuple>>>())
}

fn buffered_tuple_bytes(object_id: &str, item_hash: &str) -> usize {
    encoded_tuple_bytes(object_id, item_hash).saturating_add(mem::size_of::<BufferedTuple>())
}

fn encoded_tuple_bytes(object_id: &str, item_hash: &str) -> usize {
    object_id
        .len()
        .checked_add(item_hash.len())
        .and_then(|bytes| bytes.checked_add(mem::size_of::<u64>() * 4))
        .unwrap_or(usize::MAX)
}

fn append_partition_file(path: &Path) -> Result<File, SourceIdentityError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(SourceIdentityError::Io)
}

fn encode_tuple(
    object_id: &str,
    item_hash: &str,
    page_index: usize,
    item_index: usize,
) -> Result<Box<[u8]>, SourceIdentityError> {
    // Length prefixes keep every validated JSON string reversible, including embedded newlines
    // and NULs. One exact encoded allocation replaces per-tuple String and Vec capacity overhead.
    let mut encoded = vec![0; encoded_tuple_bytes(object_id, item_hash)].into_boxed_slice();
    let mut cursor = io::Cursor::new(encoded.as_mut());
    write_length_prefixed_string(&mut cursor, object_id)
        .and_then(|_| write_length_prefixed_string(&mut cursor, item_hash))
        .and_then(|_| write_usize(&mut cursor, page_index))
        .and_then(|_| write_usize(&mut cursor, item_index))
        .map_err(SourceIdentityError::Io)?;
    Ok(encoded)
}

fn write_length_prefixed_string(writer: &mut impl Write, value: &str) -> io::Result<()> {
    write_usize(writer, value.len())?;
    writer.write_all(value.as_bytes())
}

fn write_usize(writer: &mut impl Write, value: usize) -> io::Result<()> {
    let encoded = u64::try_from(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "spool value exceeds u64"))?;
    writer.write_all(&encoded.to_be_bytes())
}

fn read_partition_tuples(
    path: &Path,
    partition: u32,
    budget_bytes: usize,
) -> Result<PartitionLoad, SourceIdentityError> {
    let encoded = read_partition_bytes(path, partition, budget_bytes)?;
    let tuple_count = count_partition_tuples(&encoded)?;
    let offsets_bytes = tuple_count.saturating_mul(mem::size_of::<PartitionTupleOffset>());
    let resident_bytes = encoded.len().saturating_add(offsets_bytes);
    ensure_partition_budget(partition, resident_bytes, budget_bytes)?;

    let mut tuple_offsets = vec![PartitionTupleOffset::default(); tuple_count].into_boxed_slice();
    let mut cursor = PartitionCursor {
        encoded: &encoded,
        position: 0,
    };
    for tuple in &mut tuple_offsets {
        *tuple = cursor
            .next_tuple()?
            .expect("first partition pass counted every tuple");
    }
    Ok(PartitionLoad {
        encoded,
        tuple_offsets,
        resident_bytes,
    })
}

fn read_partition_bytes(
    path: &Path,
    partition: u32,
    budget_bytes: usize,
) -> Result<Box<[u8]>, SourceIdentityError> {
    let mut file = File::open(path).map_err(SourceIdentityError::Io)?;
    let file_bytes = usize::try_from(file.metadata().map_err(SourceIdentityError::Io)?.len())
        .unwrap_or(usize::MAX);
    ensure_partition_budget(partition, file_bytes, budget_bytes)?;

    let mut encoded = vec![0; file_bytes].into_boxed_slice();
    file.read_exact(&mut encoded)
        .map_err(|_| corrupt_partition_error())?;
    let mut unexpected_tail = [0];
    if file
        .read(&mut unexpected_tail)
        .map_err(SourceIdentityError::Io)?
        != 0
    {
        return Err(corrupt_partition_error());
    }
    Ok(encoded)
}

fn count_partition_tuples(encoded: &[u8]) -> Result<usize, SourceIdentityError> {
    let mut cursor = PartitionCursor {
        encoded,
        position: 0,
    };
    let mut count = 0usize;
    while cursor.next_tuple()?.is_some() {
        count = count.checked_add(1).ok_or_else(corrupt_partition_error)?;
    }
    Ok(count)
}

fn ensure_partition_budget(
    partition: u32,
    bytes: usize,
    budget_bytes: usize,
) -> Result<(), SourceIdentityError> {
    if bytes > budget_bytes {
        return Err(SourceIdentityError::PartitionBudgetExceeded {
            partition,
            bytes,
            budget_bytes,
        });
    }
    Ok(())
}

fn validated_spool_string(encoded: &[u8], start: usize, length: usize) -> &str {
    std::str::from_utf8(&encoded[start..start + length])
        .expect("partition strings were validated before offsets were allocated")
}

fn corrupt_partition_error() -> SourceIdentityError {
    SourceIdentityError::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        "corrupt source identity partition",
    ))
}

fn compare_tuples(
    encoded: &[u8],
    left: &PartitionTupleOffset,
    right: &PartitionTupleOffset,
) -> Ordering {
    left.object_id(encoded)
        .cmp(right.object_id(encoded))
        .then(left.page_index.cmp(&right.page_index))
        .then(left.item_index.cmp(&right.item_index))
}

fn identity_v2_hasher(partition_count: u32) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(IDENTITY_V2_DOMAIN);
    update_decimal_u32(&mut hasher, partition_count);
    hasher.update(*b"\n");
    hasher
}

fn update_identity_v2_digest(hasher: &mut Sha256, partition: u32, digest: [u8; 32]) {
    let mut digest_hex = [0; IDENTITY_DIGEST_HEX_BYTES];
    hex::encode_to_slice(digest, &mut digest_hex)
        .expect("sha256 digest always encodes to a 64-byte hex value");
    update_decimal_u32(hasher, partition);
    hasher.update([0]);
    hasher.update(digest_hex);
    hasher.update(*b"\n");
}

fn update_decimal_u32(hasher: &mut Sha256, value: u32) {
    let mut buffer = [0; 10];
    let mut cursor = buffer.len();
    let mut remaining = value;
    loop {
        cursor -= 1;
        buffer[cursor] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    hasher.update(&buffer[cursor..]);
}
