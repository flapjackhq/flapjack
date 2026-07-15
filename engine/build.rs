#[path = "src/build_info.rs"]
mod build_info;

use build_info::{
    collect_vcs_inputs, discover_vcs, workspace_digest, WORKSPACE_DIGEST_FILES,
    WORKSPACE_DIGEST_RUST_DIRS,
};
use std::env;
use std::path::Path;
use std::process::Command;

const BUILD_REVISION_ENV: &str = "FLAPJACK_BUILD_REVISION";
const REVISION_INPUT_ENV: &str = "FLAPJACK_INTERNAL_BUILD_REVISION";
const DIRTY_INPUT_ENV: &str = "FLAPJACK_INTERNAL_BUILD_DIRTY";
const DIGEST_INPUT_ENV: &str = "FLAPJACK_INTERNAL_WORKSPACE_DIGEST";
const PROFILE_INPUT_ENV: &str = "FLAPJACK_INTERNAL_BUILD_PROFILE";
const TARGET_INPUT_ENV: &str = "FLAPJACK_INTERNAL_BUILD_TARGET";

fn main() {
    let workspace_root = env::var("CARGO_MANIFEST_DIR")
        .map_err(|error| format!("CARGO_MANIFEST_DIR is unavailable: {error}"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|error| panic!("{error}"));

    emit_invalidation_policy();

    let revision_override = revision_override();
    let vcs = collect_vcs_inputs(revision_override.as_deref(), || {
        discover_vcs(|arguments| run_git(&workspace_root, arguments))
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let digest = workspace_digest(&workspace_root)
        .unwrap_or_else(|error| panic!("failed to calculate workspace digest: {error}"));
    let profile = required_build_input("PROFILE");
    let target = required_build_input("TARGET");

    emit_raw_input(
        REVISION_INPUT_ENV,
        vcs.revision.as_deref().unwrap_or_default(),
    );
    emit_raw_input(
        DIRTY_INPUT_ENV,
        vcs.dirty
            .map(|dirty| if dirty { "true" } else { "false" })
            .unwrap_or("unknown"),
    );
    emit_raw_input(DIGEST_INPUT_ENV, &digest);
    emit_raw_input(PROFILE_INPUT_ENV, &profile);
    emit_raw_input(TARGET_INPUT_ENV, &target);
}

fn emit_invalidation_policy() {
    println!("cargo:rerun-if-env-changed={BUILD_REVISION_ENV}");
    for path in WORKSPACE_DIGEST_FILES
        .iter()
        .chain(WORKSPACE_DIGEST_RUST_DIRS.iter())
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn run_git(workspace_root: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(workspace_root)
        .output()
        .map_err(|error| format!("could not run Git: {error}"))?;
    if !output.status.success() {
        return Err(format!("Git exited with {}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("Git returned non-UTF-8: {error}"))
}

fn required_build_input(name: &str) -> String {
    env::var(name)
        .unwrap_or_else(|error| panic!("required Cargo build input {name} is missing: {error}"))
}

fn revision_override() -> Option<String> {
    match env::var(BUILD_REVISION_ENV) {
        Ok(revision) => Some(revision),
        Err(env::VarError::NotPresent) => None,
        Err(env::VarError::NotUnicode(_)) => {
            panic!("{BUILD_REVISION_ENV} must contain UTF-8 hexadecimal characters")
        }
    }
}

fn emit_raw_input(name: &str, value: &str) {
    println!("cargo:rustc-env={name}={value}");
}
