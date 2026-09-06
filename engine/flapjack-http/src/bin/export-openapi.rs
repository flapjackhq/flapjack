//! Stub summary for export-openapi.rs.
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use flapjack_http::openapi_export::OpenApiDocument;

#[derive(Debug, PartialEq, Eq)]
struct ExportArgs {
    document: OpenApiDocument,
    output_path: PathBuf,
}

fn usage() -> String {
    "usage: cargo run -p flapjack-http --bin export-openapi [--document public|pbv4-crawler] [--output <path>]".to_string()
}

fn parse_args<I>(args: I) -> Result<ExportArgs, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut document = OpenApiDocument::Public;
    let mut output_path = None;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        if flag == OsStr::new("--document") {
            let value = args.next().ok_or_else(usage)?;
            let value = value.to_str().ok_or_else(usage)?;
            document = OpenApiDocument::parse(value).ok_or_else(usage)?;
        } else if flag == OsStr::new("--output") {
            output_path = Some(PathBuf::from(args.next().ok_or_else(usage)?));
        } else {
            return Err(usage());
        }
    }

    let output_path = output_path.unwrap_or_else(|| match document {
        OpenApiDocument::Public => flapjack_http::openapi_export::default_docs2_output_path(),
        OpenApiDocument::Pbv4Crawler => {
            flapjack_http::openapi_export::default_pbv4_crawler_output_path()
        }
    });
    Ok(ExportArgs {
        document,
        output_path,
    })
}

fn parse_process_args() -> Result<ExportArgs, String> {
    parse_args(std::env::args_os().skip(1))
}

/// TODO: Document main.
fn main() {
    let args = match parse_process_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    if let Err(error) =
        flapjack_http::openapi_export::write_openapi_document_json(args.document, &args.output_path)
    {
        eprintln!(
            "failed to export OpenAPI spec to {}: {}",
            args.output_path.display(),
            error
        );
        std::process::exit(1);
    }

    println!("wrote OpenAPI spec to {}", args.output_path.display());
}

#[cfg(test)]
mod tests {
    use super::{parse_args, ExportArgs};
    use flapjack_http::openapi_export::OpenApiDocument;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn parse_output_path_defaults_to_docs2_openapi_json() {
        assert_eq!(
            parse_args(Vec::<OsString>::new()).expect("default path should parse"),
            ExportArgs {
                document: OpenApiDocument::Public,
                output_path: flapjack_http::openapi_export::default_docs2_output_path(),
            }
        );
    }

    #[test]
    fn parse_output_path_accepts_output_flag() {
        let parsed = parse_args(vec![
            OsString::from("--output"),
            OsString::from("/tmp/custom-openapi.json"),
        ])
        .expect("custom output path should parse");

        assert_eq!(
            parsed,
            ExportArgs {
                document: OpenApiDocument::Public,
                output_path: PathBuf::from("/tmp/custom-openapi.json"),
            }
        );
    }

    #[test]
    fn parse_args_selects_the_pbv4_crawler_document_and_default_path() {
        assert_eq!(
            parse_args(vec![
                OsString::from("--document"),
                OsString::from("pbv4-crawler"),
            ])
            .expect("crawler document should parse"),
            ExportArgs {
                document: OpenApiDocument::Pbv4Crawler,
                output_path: flapjack_http::openapi_export::default_pbv4_crawler_output_path(),
            }
        );
    }

    #[test]
    fn parse_output_path_rejects_invalid_argument_shapes() {
        let error = parse_args(vec![OsString::from("--output")])
            .expect_err("missing output path should be rejected");

        assert!(
            error.contains("usage: cargo run -p flapjack-http --bin export-openapi"),
            "usage string should explain supported arguments"
        );
    }
}
