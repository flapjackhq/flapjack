use std::path::{Path, PathBuf};

/// Remove the dedicated install root and clean installer-owned shell PATH entries.
pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set")?;
    let install_dir =
        std::env::var("FLAPJACK_INSTALL").unwrap_or_else(|_| format!("{home}/.flapjack"));

    // Environment input is only a candidate. Resolve and positively identify
    // the dedicated install before giving it to a recursive delete primitive.
    let install_path = Path::new(&install_dir);
    if install_path.exists() {
        let running_binary = std::env::current_exe()?;
        let validated = validate_target(install_path, Path::new(&home), &running_binary).map_err(
            |message| std::io::Error::new(std::io::ErrorKind::PermissionDenied, message),
        )?;
        std::fs::remove_dir_all(&validated)?;
        eprintln!("Removed {}", validated.display());
    } else {
        eprintln!("Directory {install_dir} does not exist, skipping");
    }

    let rc_files = [
        format!("{home}/.bashrc"),
        format!("{home}/.bash_profile"),
        format!("{home}/.zshrc"),
        format!("{home}/.profile"),
        format!("{home}/.config/fish/config.fish"),
    ];

    for rc_path in &rc_files {
        let path = Path::new(rc_path);
        if !path.exists() {
            continue;
        }
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };
        if !contents.contains(".flapjack") {
            continue;
        }
        if let Some(new_contents) = strip_path_entries(&contents) {
            std::fs::write(path, new_contents)?;
            eprintln!("Cleaned PATH entry from {rc_path}");
        }
    }

    eprintln!("\nFlapjack has been uninstalled.");
    Ok(())
}

/// Resolve and positively identify the one directory shape created by `install.sh`.
///
/// Broad path blacklists alone are insufficient: a typo can name any valuable
/// directory. The target must instead be a dedicated root containing only a
/// `bin/flapjack` artifact byte-for-byte equal to the process performing the
/// uninstall. The explicit root/home/repository checks make the most dangerous
/// operator mistakes fail with a useful reason before artifact inspection.
pub(crate) fn validate_target(
    install_dir: &Path,
    home_dir: &Path,
    running_binary: &Path,
) -> Result<PathBuf, String> {
    let target_metadata = std::fs::symlink_metadata(install_dir)
        .map_err(|error| format!("Cannot inspect uninstall target: {error}"))?;
    if target_metadata.file_type().is_symlink() || !target_metadata.is_dir() {
        return Err("Refusing to uninstall: target must be a real directory".to_string());
    }

    let target = std::fs::canonicalize(install_dir)
        .map_err(|error| format!("Cannot resolve uninstall target: {error}"))?;
    if target.parent().is_none() {
        return Err("Refusing to uninstall the filesystem root".to_string());
    }

    let home = std::fs::canonicalize(home_dir)
        .map_err(|error| format!("Cannot resolve home directory: {error}"))?;
    if target == home {
        return Err("Refusing to uninstall the home directory".to_string());
    }

    // Worktree `.git` files and ordinary `.git` directories are both covered.
    // Cargo.toml catches source exports whose VCS metadata was intentionally
    // omitted; neither marker belongs to an installer-created root.
    if [".git", ".hg", ".svn", "Cargo.toml"]
        .iter()
        .any(|marker| target.join(marker).exists())
    {
        return Err("Refusing to uninstall a source repository".to_string());
    }

    let root_entries = directory_entry_names(&target)?;
    if root_entries.as_slice() != [std::ffi::OsString::from("bin")] {
        return Err(
            "Refusing to uninstall: target is not a dedicated Flapjack install root".to_string(),
        );
    }

    let bin_dir = target.join("bin");
    let bin_metadata = std::fs::symlink_metadata(&bin_dir)
        .map_err(|error| format!("Cannot inspect install bin directory: {error}"))?;
    if bin_metadata.file_type().is_symlink() || !bin_metadata.is_dir() {
        return Err("Refusing to uninstall: bin must be a real directory".to_string());
    }

    let bin_entries = directory_entry_names(&bin_dir)?;
    if bin_entries.as_slice() != [std::ffi::OsString::from("flapjack")] {
        return Err(
            "Refusing to uninstall: expected the dedicated bin/flapjack artifact".to_string(),
        );
    }

    let installed_binary = bin_dir.join("flapjack");
    let binary_metadata = std::fs::symlink_metadata(&installed_binary)
        .map_err(|error| format!("Cannot inspect bin/flapjack: {error}"))?;
    if binary_metadata.file_type().is_symlink() || !binary_metadata.is_file() {
        return Err("Refusing to uninstall: bin/flapjack must be a regular file".to_string());
    }
    if !files_have_same_contents(&installed_binary, running_binary)? {
        return Err(
            "Refusing to uninstall: bin/flapjack does not match the running executable".to_string(),
        );
    }

    Ok(target)
}

fn directory_entry_names(directory: &Path) -> Result<Vec<std::ffi::OsString>, String> {
    let mut names = std::fs::read_dir(directory)
        .map_err(|error| format!("Cannot inspect {}: {error}", directory.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name())
                .map_err(|error| format!("Cannot inspect {}: {error}", directory.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    Ok(names)
}

fn files_have_same_contents(left: &Path, right: &Path) -> Result<bool, String> {
    use std::io::BufRead;

    let left_file = std::fs::File::open(left)
        .map_err(|error| format!("Cannot read {}: {error}", left.display()))?;
    let right_file = std::fs::File::open(right)
        .map_err(|error| format!("Cannot read running executable: {error}"))?;
    if left_file
        .metadata()
        .map_err(|error| error.to_string())?
        .len()
        != right_file
            .metadata()
            .map_err(|error| error.to_string())?
            .len()
    {
        return Ok(false);
    }

    let mut left_reader = std::io::BufReader::new(left_file);
    let mut right_reader = std::io::BufReader::new(right_file);
    loop {
        let left_buffer = left_reader
            .fill_buf()
            .map_err(|error| format!("Cannot read {}: {error}", left.display()))?;
        let right_buffer = right_reader
            .fill_buf()
            .map_err(|error| format!("Cannot read running executable: {error}"))?;
        if left_buffer.is_empty() || right_buffer.is_empty() {
            return Ok(left_buffer.is_empty() && right_buffer.is_empty());
        }
        let compared = left_buffer.len().min(right_buffer.len());
        if left_buffer[..compared] != right_buffer[..compared] {
            return Ok(false);
        }
        left_reader.consume(compared);
        right_reader.consume(compared);
    }
}

/// Remove installer marker blocks and `.flapjack` PATH lines, returning `None` when unchanged.
pub(crate) fn strip_path_entries(contents: &str) -> Option<String> {
    let mut new_lines: Vec<&str> = Vec::new();
    let mut lines = contents.lines().peekable();
    let mut modified = false;

    while let Some(line) = lines.next() {
        if line.trim() == "# Flapjack" {
            if matches!(lines.peek(), Some(next_line) if next_line.contains(".flapjack")) {
                lines.next();
            }
            modified = true;
            continue;
        }
        if is_flapjack_path_line(line) {
            modified = true;
            continue;
        }
        new_lines.push(line);
    }

    if !modified {
        return None;
    }
    while new_lines.last() == Some(&"") {
        new_lines.pop();
    }
    let mut new_contents = new_lines.join("\n");
    if !new_contents.is_empty() {
        new_contents.push('\n');
    }
    Some(new_contents)
}

fn is_flapjack_path_line(line: &str) -> bool {
    (line.contains("export PATH") || line.contains("set -gx PATH")) && line.contains(".flapjack")
}
