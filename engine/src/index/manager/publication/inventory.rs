use super::{invalid_publication, Result, TantivyManagedInventory};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

impl TantivyManagedInventory {
    /// Build inventory evidence from the files present in publication transaction trees.
    pub fn from_existing_trees<'a>(roots: impl IntoIterator<Item = &'a Path>) -> Result<Self> {
        let mut files = BTreeSet::new();
        for root in roots {
            reject_symlinked_inventory_root(root)?;
            collect_relative_files(root, root, &mut files)?;
        }
        Self::new(files)
    }
}

fn reject_symlinked_inventory_root(root: &Path) -> Result<()> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(invalid_publication(format!(
            "refusing symlinked tenant inventory root '{}'",
            root.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn collect_relative_files(
    root: &Path,
    current: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(current)? {
        let path = entry?.path();
        if crate::index::utils::is_temporary_entry(&path) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid_publication(format!(
                "refusing symlinked tenant artifact '{}'",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_relative_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root).map_err(|_| {
                invalid_publication(format!(
                    "tenant artifact '{}' escapes publication tree '{}'",
                    path.display(),
                    root.display()
                ))
            })?;
            files.insert(relative.to_path_buf());
        } else {
            return Err(invalid_publication(format!(
                "refusing unsupported tenant artifact '{}'",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn inventory_ignores_atomic_write_temporary_files() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("tenant");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index_meta.json"), b"published").unwrap();
        std::fs::write(
            root.join(".tmp.index_meta.json.123.456.0.tmp"),
            b"in flight",
        )
        .unwrap();
        std::fs::write(root.join(".index_meta.json.tmp"), b"legacy metadata temp").unwrap();
        std::fs::write(
            root.join(".committed_seq.42.99.tmp"),
            b"legacy watermark temp",
        )
        .unwrap();

        assert_eq!(
            TantivyManagedInventory::from_existing_trees([root.as_path()]).unwrap(),
            TantivyManagedInventory::new([PathBuf::from("index_meta.json")]).unwrap(),
            "publication inventory must exclude current and legacy atomic-write temporary files"
        );
    }
}
