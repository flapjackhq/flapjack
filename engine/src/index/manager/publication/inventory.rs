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
