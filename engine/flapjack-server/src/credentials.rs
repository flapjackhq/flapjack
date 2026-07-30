//! Shared CLI secret-source and HTTP header validation.

use std::io::{self, Read};
use std::path::Path;

pub(crate) struct SecretSource<'a> {
    env_var: Option<&'a str>,
    file: Option<&'a Path>,
    stdin: bool,
}

impl<'a> SecretSource<'a> {
    pub(crate) fn new(env_var: Option<&'a str>, file: Option<&'a Path>, stdin: bool) -> Self {
        Self {
            env_var,
            file,
            stdin,
        }
    }

    pub(crate) fn validate_exactly_one(&self, flags: &str) -> Result<(), String> {
        let source_count = usize::from(self.env_var.is_some())
            + usize::from(self.file.is_some())
            + usize::from(self.stdin);
        if source_count != 1 {
            return Err(format!("exactly one of {flags} is required"));
        }
        Ok(())
    }

    pub(crate) fn read(&self, secret_name: &str) -> Result<String, String> {
        let secret = if let Some(env_var) = self.env_var {
            std::env::var(env_var)
                .map_err(|_| format!("{secret_name} environment variable is not set"))?
        } else if let Some(path) = self.file {
            std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read {secret_name} file: {error}"))?
        } else {
            let mut secret = String::new();
            io::stdin()
                .read_to_string(&mut secret)
                .map_err(|error| format!("failed to read {secret_name} from stdin: {error}"))?;
            secret
        };
        let secret = secret.trim().to_string();
        validate_required_http_header_value(secret_name, &secret)?;
        Ok(secret)
    }
}

pub(crate) fn validate_http_header_value(name: &str, value: &str) -> Result<(), String> {
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(format!("{name} cannot contain HTTP control characters"));
    }
    Ok(())
}

pub(crate) fn validate_required_http_header_value(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{name} cannot be empty"));
    }
    validate_http_header_value(name, value)
}
