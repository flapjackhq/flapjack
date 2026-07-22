use std::path::Path;

pub(crate) fn dashboard_dist_has_real_assets(dist_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dist_dir.join("assets")) else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_type()
            .map(|file_type| file_type.is_file())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::dashboard_dist_has_real_assets;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn create_dist_with_index(temp_dir: &TempDir) -> &Path {
        let dist_dir = temp_dir.path().join("dist");
        fs::create_dir(&dist_dir).expect("create dist directory");
        fs::write(dist_dir.join("index.html"), "<!doctype html>").expect("write index");
        temp_dir.path()
    }

    #[test]
    fn dashboard_build_accepts_dist_with_direct_asset_file() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let root = create_dist_with_index(&temp_dir);
        let assets_dir = root.join("dist").join("assets");
        fs::create_dir(&assets_dir).expect("create assets directory");
        fs::write(assets_dir.join("app.js"), "console.log('flapjack');").expect("write asset");

        assert!(dashboard_dist_has_real_assets(&root.join("dist")));
    }

    #[test]
    fn dashboard_build_rejects_index_only_placeholder_dist() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let root = create_dist_with_index(&temp_dir);

        assert!(!dashboard_dist_has_real_assets(&root.join("dist")));
    }

    #[test]
    fn dashboard_build_rejects_empty_assets_directory() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let root = create_dist_with_index(&temp_dir);
        fs::create_dir(root.join("dist").join("assets")).expect("create assets directory");

        assert!(!dashboard_dist_has_real_assets(&root.join("dist")));
    }

    #[test]
    fn dashboard_build_rejects_assets_directory_with_only_nested_directory() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let root = create_dist_with_index(&temp_dir);
        fs::create_dir_all(root.join("dist").join("assets").join("nested"))
            .expect("create nested assets directory");

        assert!(!dashboard_dist_has_real_assets(&root.join("dist")));
    }
}
