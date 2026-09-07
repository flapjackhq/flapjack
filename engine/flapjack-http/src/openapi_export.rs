use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use utoipa::OpenApi;

use crate::openapi::{pbv4_crawler_openapi, ApiDoc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenApiDocument {
    Public,
    Pbv4Crawler,
}

impl OpenApiDocument {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "public" => Some(Self::Public),
            "pbv4-crawler" => Some(Self::Pbv4Crawler),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum OpenApiExportError {
    Serialize(serde_json::Error),
    CreateParentDir(std::io::Error),
    WriteTempFile(std::io::Error),
    ReplaceTarget(std::io::Error),
}

impl Display for OpenApiExportError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(error) => write!(f, "failed to serialize OpenAPI JSON: {error}"),
            Self::CreateParentDir(error) => {
                write!(f, "failed to create OpenAPI output directory: {error}")
            }
            Self::WriteTempFile(error) => {
                write!(f, "failed to write temporary OpenAPI file: {error}")
            }
            Self::ReplaceTarget(error) => {
                write!(f, "failed to replace OpenAPI output file: {error}")
            }
        }
    }
}

impl std::error::Error for OpenApiExportError {}

pub fn default_docs2_output_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs2")
        .join("openapi.json")
}

pub fn default_pbv4_crawler_output_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs2")
        .join("pbv4-crawler-openapi.json")
}

pub fn write_openapi_json(output_path: &Path) -> Result<(), OpenApiExportError> {
    write_openapi_document_json(OpenApiDocument::Public, output_path)
}

pub fn write_openapi_document_json(
    document: OpenApiDocument,
    output_path: &Path,
) -> Result<(), OpenApiExportError> {
    let openapi_json = match document {
        OpenApiDocument::Public => serde_json::to_string_pretty(&ApiDoc::openapi()),
        OpenApiDocument::Pbv4Crawler => serde_json::to_string_pretty(&pbv4_crawler_openapi()),
    }
    .map_err(OpenApiExportError::Serialize)?;

    if let Some(parent_dir) = output_path.parent() {
        std::fs::create_dir_all(parent_dir).map_err(OpenApiExportError::CreateParentDir)?;
    }

    let temp_path = output_path.with_extension("json.tmp");
    std::fs::write(&temp_path, openapi_json.as_bytes())
        .map_err(OpenApiExportError::WriteTempFile)?;
    std::fs::rename(&temp_path, output_path).map_err(OpenApiExportError::ReplaceTarget)?;
    Ok(())
}
