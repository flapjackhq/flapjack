use super::{sorted_segment_entries, OpLogEntry, OPLOG_TASK_ID_FIELD};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

pub(super) struct RetractionOutcome {
    pub(super) removed: u64,
    pub(super) changed: bool,
}

struct SuffixScanState {
    started: bool,
    last_retained_seq: u64,
}

struct SegmentRetraction {
    retained_bytes: u64,
    rejected_ranges: Vec<std::ops::Range<u64>>,
    removed: u64,
}

pub(super) fn retract_suffix_segments(
    dir: &Path,
    from_seq: u64,
) -> crate::error::Result<RetractionOutcome> {
    let mut outcome = RetractionOutcome {
        removed: 0,
        changed: false,
    };
    let mut scan_state = SuffixScanState {
        started: false,
        last_retained_seq: 0,
    };
    for entry in sorted_segment_entries(dir)
        .map_err(|error| oplog_io_error("enumerate oplog segments for retraction", dir, error))?
    {
        let path = entry.path();
        let segment = scan_suffix_segment(&path, from_seq, &mut scan_state)?;
        if segment.rejected_ranges.is_empty() {
            continue;
        }
        outcome.changed = true;
        outcome.removed += segment.removed;
        if segment.retained_bytes == 0 {
            remove_segment_file(&path)?;
        } else {
            truncate_segment_file(&path, segment.retained_bytes)?;
        }
    }
    Ok(outcome)
}

fn scan_suffix_segment(
    path: &Path,
    from_seq: u64,
    state: &mut SuffixScanState,
) -> crate::error::Result<SegmentRetraction> {
    let file = File::open(path)
        .map_err(|error| oplog_io_error("open oplog segment for retraction", path, error))?;
    let mut reader = BufReader::new(file);
    let mut segment = SegmentRetraction {
        retained_bytes: 0,
        rejected_ranges: Vec::new(),
        removed: 0,
    };
    loop {
        let mut line = Vec::new();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(|error| oplog_io_error("read oplog segment for retraction", path, error))?
            == 0
        {
            break;
        }
        match serde_json::from_slice::<OpLogEntry>(&line) {
            Ok(op) if state.started || op.seq >= from_seq => {
                state.started = true;
                segment.removed += 1;
                segment.rejected_ranges.push(0..line.len() as u64);
            }
            Ok(op) => {
                state.last_retained_seq = state.last_retained_seq.max(op.seq);
                segment.retained_bytes += line.len() as u64;
            }
            Err(_) if state.started || from_seq <= state.last_retained_seq.saturating_add(1) => {
                state.started = true;
                segment.rejected_ranges.push(0..line.len() as u64);
            }
            Err(_) => segment.retained_bytes += line.len() as u64,
        }
    }
    Ok(segment)
}

pub(super) fn retract_task_segments(
    dir: &Path,
    from_seq: u64,
    task_ids: &BTreeSet<String>,
) -> crate::error::Result<RetractionOutcome> {
    let mut outcome = RetractionOutcome {
        removed: 0,
        changed: false,
    };
    let mut scan_state = SuffixScanState {
        started: false,
        last_retained_seq: 0,
    };
    for entry in sorted_segment_entries(dir).map_err(|error| {
        oplog_io_error("enumerate oplog segments for task retraction", dir, error)
    })? {
        let path = entry.path();
        let (file_bytes, segment) = scan_task_segment(&path, from_seq, task_ids, &mut scan_state)?;
        if segment.rejected_ranges.is_empty() {
            continue;
        }
        outcome.changed = true;
        outcome.removed += segment.removed;
        apply_task_segment_retraction(&path, file_bytes.len() as u64, &segment.rejected_ranges)?;
    }
    Ok(outcome)
}

fn scan_task_segment(
    path: &Path,
    from_seq: u64,
    task_ids: &BTreeSet<String>,
    state: &mut SuffixScanState,
) -> crate::error::Result<(Vec<u8>, SegmentRetraction)> {
    let mut file_bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut file_bytes))
        .map_err(|error| oplog_io_error("read oplog segment for task retraction", path, error))?;
    let mut segment = SegmentRetraction {
        retained_bytes: 0,
        rejected_ranges: Vec::new(),
        removed: 0,
    };
    let mut line_start = 0u64;
    for line in file_bytes.split_inclusive(|byte| *byte == b'\n') {
        let line_end = line_start + line.len() as u64;
        match serde_json::from_slice::<OpLogEntry>(line) {
            Ok(op) if op.seq >= from_seq && op_has_task_id(&op, task_ids) => {
                state.started = true;
                segment.removed += 1;
                segment.rejected_ranges.push(line_start..line_end);
            }
            Ok(op) => state.last_retained_seq = state.last_retained_seq.max(op.seq),
            Err(_) if state.started || from_seq <= state.last_retained_seq.saturating_add(1) => {
                state.started = true;
                segment.rejected_ranges.push(line_start..line_end);
            }
            Err(_) => {}
        }
        line_start = line_end;
    }
    Ok((file_bytes, segment))
}

fn apply_task_segment_retraction(
    path: &Path,
    file_bytes: u64,
    rejected_ranges: &[std::ops::Range<u64>],
) -> crate::error::Result<()> {
    match contiguous_rejected_suffix_start(rejected_ranges, file_bytes) {
        Some(0) => remove_segment_file(path),
        Some(suffix_start) => truncate_segment_file(path, suffix_start),
        None => neutralize_segment_file_ranges(path, rejected_ranges),
    }
}

pub(super) fn op_has_task_id(op: &OpLogEntry, task_ids: &BTreeSet<String>) -> bool {
    op.payload
        .get(OPLOG_TASK_ID_FIELD)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|task_id| task_ids.contains(task_id))
}

/// Durably remove a segment suffix without allocating replacement space.
/// `retract_from` only ever discards a suffix, so truncating the existing file
/// is both the direct operation and safe under the ENOSPC condition that
/// initiated compensation.
pub(super) fn truncate_segment_file(path: &Path, retained_bytes: u64) -> crate::error::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| oplog_io_error("open oplog segment for suffix truncation", path, error))?;
    let original_bytes = file
        .metadata()
        .map_err(|error| oplog_io_error("read oplog segment length", path, error))?
        .len();
    let truncate_result = file.set_len(retained_bytes);
    complete_segment_suffix_retraction(
        &mut file,
        path,
        retained_bytes..original_bytes,
        truncate_result,
    )
}

pub(super) fn neutralize_segment_file_ranges(
    path: &Path,
    rejected_ranges: &[std::ops::Range<u64>],
) -> crate::error::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| oplog_io_error("open oplog segment for task retraction", path, error))?;
    neutralize_segment_ranges(&mut file, path, rejected_ranges)?;
    file.sync_all()
        .map_err(|error| oplog_io_error("sync neutralized oplog segment", path, error))?;
    Ok(())
}

pub(super) fn contiguous_rejected_suffix_start(
    rejected_ranges: &[std::ops::Range<u64>],
    file_bytes: u64,
) -> Option<u64> {
    let mut expected_end = file_bytes;
    for rejected_range in rejected_ranges.iter().rev() {
        if rejected_range.end != expected_end {
            return None;
        }
        expected_end = rejected_range.start;
    }
    Some(expected_end)
}

pub(super) fn neutralize_segment_ranges<W: Seek + Write>(
    writer: &mut W,
    path: &Path,
    rejected_ranges: &[std::ops::Range<u64>],
) -> crate::error::Result<()> {
    let neutral_bytes = [b' '; 8 * 1024];
    for rejected_range in rejected_ranges {
        let rejected_bytes = rejected_range
            .end
            .checked_sub(rejected_range.start)
            .filter(|bytes| *bytes > 0)
            .ok_or_else(|| {
                oplog_io_error(
                    "neutralize rejected oplog row",
                    path,
                    std::io::Error::from(std::io::ErrorKind::InvalidInput),
                )
            })?;
        writer
            .seek(SeekFrom::Start(rejected_range.start))
            .map_err(|error| oplog_io_error("seek to rejected oplog row", path, error))?;
        let mut remaining_spaces = rejected_bytes - 1;
        while remaining_spaces > 0 {
            let write_size = remaining_spaces.min(neutral_bytes.len() as u64) as usize;
            writer
                .write_all(&neutral_bytes[..write_size])
                .map_err(|error| oplog_io_error("neutralize rejected oplog row", path, error))?;
            remaining_spaces -= write_size as u64;
        }
        writer
            .write_all(b"\n")
            .map_err(|error| oplog_io_error("terminate neutralized oplog row", path, error))?;
    }
    Ok(())
}

pub(super) fn complete_segment_suffix_retraction(
    file: &mut File,
    path: &Path,
    rejected_suffix: std::ops::Range<u64>,
    truncate_result: std::io::Result<()>,
) -> crate::error::Result<()> {
    match truncate_result {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::StorageFull => {
            neutralize_segment_suffix(file, path, rejected_suffix)?;
        }
        Err(error) => {
            return Err(oplog_io_error("truncate oplog segment suffix", path, error));
        }
    }
    file.sync_all()
        .map_err(|error| oplog_io_error("sync truncated oplog segment", path, error))?;
    Ok(())
}

pub(super) fn neutralize_segment_suffix(
    file: &mut File,
    path: &Path,
    rejected_suffix: std::ops::Range<u64>,
) -> crate::error::Result<()> {
    let suffix_bytes = rejected_suffix.end.saturating_sub(rejected_suffix.start);
    if suffix_bytes == 0 {
        return Err(oplog_io_error(
            "truncate oplog segment suffix",
            path,
            std::io::Error::from(std::io::ErrorKind::StorageFull),
        ));
    }

    neutralize_segment_ranges(file, path, &[rejected_suffix])
}

pub(super) fn oplog_io_error(
    operation: &str,
    path: &Path,
    error: std::io::Error,
) -> crate::error::FlapjackError {
    crate::error::FlapjackError::Io(format!(
        "failed to {operation} at {}: {error}",
        path.display()
    ))
}

pub(super) fn remove_segment_file(path: &Path) -> crate::error::Result<()> {
    fs::remove_file(path)?;
    sync_parent_directory(path)
}

pub(super) fn sync_parent_directory(path: &Path) -> crate::error::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("segment path has no parent: {}", path.display()),
        )
    })?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
