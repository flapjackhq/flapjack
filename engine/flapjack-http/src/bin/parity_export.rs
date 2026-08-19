//! Stub summary for parity_export.rs.
use std::ffi::{OsStr, OsString};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use flapjack_http::mutation_parity::{self, MutationParityExportRow};

const DEFAULT_OUTPUT_FILE_NAME: &str = "algolia_parity_cases.json";

#[derive(Debug)]
enum ParityExportError {
    Serialize(serde_json::Error),
    CreateParentDir(std::io::Error),
    WriteTempFile(std::io::Error),
    ReplaceTarget(std::io::Error),
}

impl Display for ParityExportError {
    /// TODO: Document ParityExportError.fmt.
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(error) => write!(f, "failed to serialize parity export JSON: {error}"),
            Self::CreateParentDir(error) => {
                write!(
                    f,
                    "failed to create parity export output directory: {error}"
                )
            }
            Self::WriteTempFile(error) => {
                write!(f, "failed to write temporary parity export file: {error}")
            }
            Self::ReplaceTarget(error) => {
                write!(f, "failed to replace parity export output file: {error}")
            }
        }
    }
}

impl std::error::Error for ParityExportError {}

fn usage_message() -> String {
    format!(
        "usage: cargo run --manifest-path engine/Cargo.toml -p flapjack-http --bin parity_export -- [--output <path>] (default output path: {})",
        default_output_path().display()
    )
}

fn default_output_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs2")
        .join(DEFAULT_OUTPUT_FILE_NAME)
}

fn parse_output_path_from_args<I>(args: I) -> Result<PathBuf, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    match (args.next(), args.next(), args.next()) {
        (None, None, None) => Ok(default_output_path()),
        (Some(flag), Some(path), None) if flag == OsStr::new("--output") => Ok(PathBuf::from(path)),
        _ => Err(usage_message()),
    }
}

fn parse_output_path() -> Result<PathBuf, String> {
    parse_output_path_from_args(std::env::args_os().skip(1))
}

fn to_algolia_parity_json_rows() -> Vec<MutationParityExportRow> {
    mutation_parity::exported_algolia_parity_cases()
}

fn write_parity_export_json(output_path: &Path) -> Result<(), ParityExportError> {
    let json_rows = to_algolia_parity_json_rows();
    let payload = serde_json::to_string_pretty(&json_rows).map_err(ParityExportError::Serialize)?;

    if let Some(parent_dir) = output_path.parent() {
        std::fs::create_dir_all(parent_dir).map_err(ParityExportError::CreateParentDir)?;
    }

    let temp_path = output_path.with_extension("json.tmp");
    std::fs::write(&temp_path, payload.as_bytes()).map_err(ParityExportError::WriteTempFile)?;
    std::fs::rename(&temp_path, output_path).map_err(ParityExportError::ReplaceTarget)?;
    Ok(())
}

/// TODO: Document main.
fn main() {
    let output_path = match parse_output_path() {
        Ok(path) => path,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    if let Err(error) = write_parity_export_json(&output_path) {
        eprintln!(
            "failed to export parity cases to {}: {}",
            output_path.display(),
            error
        );
        std::process::exit(1);
    }

    println!(
        "wrote {} Algolia parity cases to {}",
        to_algolia_parity_json_rows().len(),
        output_path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::parse_output_path_from_args;
    use super::{default_output_path, to_algolia_parity_json_rows, usage_message};
    use flapjack_http::mutation_parity::{MutationParityKind, HIGH_RISK_MUTATION_PARITY_CASES};
    use serde_json::Value;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn parse_output_path_defaults_to_deterministic_path() {
        assert_eq!(
            parse_output_path_from_args(Vec::<OsString>::new()).expect("default path should parse"),
            default_output_path()
        );
    }

    #[test]
    fn parse_output_path_accepts_output_flag() {
        let parsed = parse_output_path_from_args(vec![
            OsString::from("--output"),
            OsString::from("/tmp/parity-export.json"),
        ])
        .expect("custom output path should parse");

        assert_eq!(parsed, PathBuf::from("/tmp/parity-export.json"));
    }

    #[test]
    fn parse_output_path_rejects_invalid_argument_shapes() {
        let error = parse_output_path_from_args(vec![OsString::from("--output")])
            .expect_err("missing output path should be rejected");

        assert!(
            error.contains(&usage_message()),
            "usage string should explain supported arguments"
        );
        assert!(
            error.contains(&default_output_path().display().to_string()),
            "usage message should include deterministic default output path"
        );
    }

    #[test]
    fn exported_row_count_matches_algolia_inventory_count() {
        let exported_rows = to_algolia_parity_json_rows();
        let expected_rows = HIGH_RISK_MUTATION_PARITY_CASES
            .iter()
            .filter(|case| case.parity_kind == MutationParityKind::AlgoliaParity)
            .count();

        assert_eq!(
            exported_rows.len(),
            expected_rows,
            "exported row count should stay locked to Algolia parity inventory"
        );
    }

    #[test]
    fn usage_message_documents_cargo_separator_before_binary_args() {
        let usage = usage_message();
        assert!(
            usage.contains("--bin parity_export -- [--output <path>]"),
            "usage should include cargo's `--` argument separator before binary args"
        );
    }

    #[test]
    fn checked_in_default_export_matches_rust_ssot_rows() {
        let checked_in_payload = std::fs::read_to_string(default_output_path())
            .expect("checked-in parity export artifact should be readable");
        let checked_in_json: Value = serde_json::from_str(&checked_in_payload)
            .expect("checked-in parity export artifact should be valid JSON");
        let expected_json = serde_json::to_value(to_algolia_parity_json_rows())
            .expect("exported Rust SSOT rows should be serializable");

        assert_eq!(
            checked_in_json, expected_json,
            "checked-in default parity export artifact must stay synchronized with Rust SSOT rows"
        );
    }
}
