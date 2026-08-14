use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub enum PermissionFailureMode {
    WarnAndContinue,
    ReturnError,
}

pub fn persist_admin_key_file(
    admin_key_file: &Path,
    key: &str,
    permission_mode: PermissionFailureMode,
) -> Result<(), String> {
    flapjack::index::atomic_write_private_file(admin_key_file, key.as_bytes())
        .map_err(|error| format!("Failed to write .admin_key: {}", error))?;

    ensure_admin_key_permissions(admin_key_file, permission_mode)
}

/// Sets the admin key file to mode 0600 (owner-only read/write) on Unix systems,
/// logging or failing based on the configured permission failure mode.
pub fn ensure_admin_key_permissions(
    admin_key_file: &Path,
    permission_mode: PermissionFailureMode,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Err(error) =
            std::fs::set_permissions(admin_key_file, std::fs::Permissions::from_mode(0o600))
        {
            return handle_permission_error(permission_mode, error.to_string());
        }
    }

    Ok(())
}

fn handle_permission_error(
    permission_mode: PermissionFailureMode,
    error_message: String,
) -> Result<(), String> {
    let message = format!("Failed to set .admin_key permissions: {}", error_message);
    match permission_mode {
        PermissionFailureMode::WarnAndContinue => {
            tracing::warn!("{}", message);
            Ok(())
        }
        PermissionFailureMode::ReturnError => Err(message),
    }
}
