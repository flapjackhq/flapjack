//! Stub summary for export-openapi.rs.
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

fn parse_output_path_from_args<I>(args: I) -> Result<PathBuf, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    match (args.next(), args.next(), args.next()) {
        (None, None, None) => Ok(flapjack_http::openapi_export::default_docs2_output_path()),
        (Some(flag), Some(path), None) if flag == OsStr::new("--output") => Ok(PathBuf::from(path)),
        _ => Err(
            "usage: cargo run -p flapjack-http --bin export-openapi [--output <path>]".to_string(),
        ),
    }
}

fn parse_output_path() -> Result<PathBuf, String> {
    parse_output_path_from_args(std::env::args_os().skip(1))
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

    if let Err(error) = flapjack_http::openapi_export::write_openapi_json(&output_path) {
        eprintln!(
            "failed to export OpenAPI spec to {}: {}",
            output_path.display(),
            error
        );
        std::process::exit(1);
    }

    println!("wrote OpenAPI spec to {}", output_path.display());
}

#[cfg(test)]
mod tests {
    use super::parse_output_path_from_args;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn parse_output_path_defaults_to_docs2_openapi_json() {
        assert_eq!(
            parse_output_path_from_args(Vec::<OsString>::new()).expect("default path should parse"),
            flapjack_http::openapi_export::default_docs2_output_path()
        );
    }

    #[test]
    fn parse_output_path_accepts_output_flag() {
        let parsed = parse_output_path_from_args(vec![
            OsString::from("--output"),
            OsString::from("/tmp/custom-openapi.json"),
        ])
        .expect("custom output path should parse");

        assert_eq!(parsed, PathBuf::from("/tmp/custom-openapi.json"));
    }

    #[test]
    fn parse_output_path_rejects_invalid_argument_shapes() {
        let error = parse_output_path_from_args(vec![OsString::from("--output")])
            .expect_err("missing output path should be rejected");

        assert!(
            error.contains("usage: cargo run -p flapjack-http --bin export-openapi"),
            "usage string should explain supported arguments"
        );
    }
}
