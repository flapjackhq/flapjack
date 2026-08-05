use std::path::Path;

use crate::startup::TlsPaths;

pub(super) const FULLCHAIN_FILE_NAME: &str = "fullchain.pem";
pub(super) const PRIVATE_KEY_FILE_NAME: &str = "privkey.pem";

pub(super) fn configured_pair_is_within_material_dir(
    paths: &TlsPaths,
    material_dir: &Path,
) -> Result<bool, String> {
    let material_dir = canonicalize_path(material_dir, "TLS material directory")?;
    Ok(
        canonicalize_path(&paths.cert_path, "TLS certificate")?.starts_with(&material_dir)
            && canonicalize_path(&paths.key_path, "TLS private key")?.starts_with(&material_dir),
    )
}

pub(super) fn configured_pair_resolves_to_generation(
    paths: &TlsPaths,
    generation: &Path,
) -> Result<bool, String> {
    let expected_cert = generation.join(FULLCHAIN_FILE_NAME);
    let expected_key = generation.join(PRIVATE_KEY_FILE_NAME);
    Ok(canonicalize_path(&paths.cert_path, "TLS certificate")?
        == canonicalize_path(&expected_cert, "TLS material generation certificate")?
        && canonicalize_path(&paths.key_path, "TLS private key")?
            == canonicalize_path(&expected_key, "TLS material generation private key")?)
}

fn canonicalize_path(path: &Path, kind: &str) -> Result<std::path::PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("failed to resolve {kind} {}: {error}", path.display()))
}
