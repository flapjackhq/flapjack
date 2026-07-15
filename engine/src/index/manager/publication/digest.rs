use super::{
    classify_tenant_relative_path, invalid_publication, ContentDigest, Result,
    TantivyManagedInventory,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Compute the canonical publication digest for a tenant tree.
pub fn canonical_tenant_tree_digest(
    root: &Path,
    inventory: &TantivyManagedInventory,
) -> Result<ContentDigest> {
    let mut records = Vec::new();
    collect_tree_records(root, root, inventory, &mut records)?;
    records.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let mut hasher = Sha256::new();
    for record in records {
        write_len_prefixed(&mut hasher, record.relative_path.as_bytes());
        hasher.update([record.entry_type]);
        write_len_prefixed(&mut hasher, &record.bytes);
    }
    ContentDigest::new(format!("sha256:{:x}", hasher.finalize()))
}

struct DigestRecord {
    relative_path: String,
    entry_type: u8,
    bytes: Vec<u8>,
}

fn collect_tree_records(
    root: &Path,
    current: &Path,
    inventory: &TantivyManagedInventory,
    records: &mut Vec<DigestRecord>,
) -> Result<()> {
    if !current.exists() {
        return Err(invalid_publication(format!(
            "digest root '{}' does not exist",
            current.display()
        )));
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = normalize_digest_relative_path(root, &path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid_publication(format!(
                "refusing symlink publication artifact '{}'",
                relative.display()
            )));
        }
        if metadata.is_dir() {
            validate_directory_artifact(&relative, inventory)?;
            records.push(DigestRecord {
                relative_path: path_to_digest_key(&relative)?,
                entry_type: b'd',
                bytes: Vec::new(),
            });
            collect_tree_records(root, &path, inventory, records)?;
        } else if metadata.is_file() {
            classify_tenant_relative_path(&relative, inventory)?;
            records.push(DigestRecord {
                relative_path: path_to_digest_key(&relative)?,
                entry_type: b'f',
                bytes: fs::read(&path)?,
            });
        } else {
            return Err(invalid_publication(format!(
                "unsupported publication artifact '{}'",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn validate_directory_artifact(relative: &Path, inventory: &TantivyManagedInventory) -> Result<()> {
    if inventory.has_descendant(relative)
        || classify_tenant_relative_path(relative, inventory).is_ok()
    {
        return Ok(());
    }
    Err(super::unknown_artifact(relative))
}

fn normalize_digest_relative_path(root: &Path, path: &Path) -> Result<PathBuf> {
    let relative = path.strip_prefix(root).map_err(|_| {
        invalid_publication(format!(
            "publication artifact '{}' escaped digest root '{}'",
            path.display(),
            root.display()
        ))
    })?;
    super::validate_relative_path("publication digest path", relative)?;
    Ok(relative.to_path_buf())
}

fn path_to_digest_key(path: &Path) -> Result<String> {
    path.to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or_else(|| invalid_publication("publication digest path is not UTF-8"))
}

fn write_len_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}
