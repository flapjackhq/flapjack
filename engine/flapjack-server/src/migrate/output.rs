use super::{MigrationCount, MigrationFailure, MigrationStatus};
use serde_json::{json, Value};

pub(super) fn finish_failure(
    failure: MigrationFailure,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let message = redact_secrets(&failure.message, &failure.secrets);
    if let Some(status) = failure.status.as_ref() {
        if json_output {
            print_failure_status(status, &failure, &message)?;
        } else {
            print_status(status, json_output, &failure.secrets)?;
        }
    } else if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "errorType": failure.kind.label(),
                "message": message,
                "exitCode": failure.kind.exit_code()
            }))?
        );
    }
    eprintln!("{message}");
    std::process::exit(failure.kind.exit_code());
}

fn print_failure_status(
    status: &MigrationStatus,
    failure: &MigrationFailure,
    message: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut report = serde_json::to_value(redacted_status(status, &failure.secrets))?;
    let Some(fields) = report.as_object_mut() else {
        return Err("migration status did not serialize as a JSON object".into());
    };
    fields.insert(
        "errorType".to_string(),
        Value::String(failure.kind.label().to_string()),
    );
    fields.insert("message".to_string(), Value::String(message.to_string()));
    fields.insert(
        "exitCode".to_string(),
        Value::Number(failure.kind.exit_code().into()),
    );
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

pub(super) fn print_status(
    status: &MigrationStatus,
    json_output: bool,
    secrets: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let status = redacted_status(status, secrets);
    if json_output {
        println!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    let mut fields = vec![
        format!("job_id={}", status.job_id),
        format!("phase={}", status.phase),
        format!("disposition={}", status.disposition),
    ];
    if let Some(target_index) = status.target_index.as_deref() {
        fields.push(format!("target_index={target_index}"));
    }
    if let Some(topology) = status.topology.as_deref() {
        fields.push(format!("topology={topology}"));
    }
    if let Some(settings_applied) = status.settings_applied {
        fields.push(format!("settings_applied={settings_applied}"));
    }
    append_count(
        &mut fields,
        "objects_imported",
        status.objects_imported.as_ref(),
    );
    append_count(
        &mut fields,
        "synonyms_imported",
        status.synonyms_imported.as_ref(),
    );
    append_count(
        &mut fields,
        "rules_imported",
        status.rules_imported.as_ref(),
    );
    println!("{}", fields.join(" "));
    for warning in &status.warnings {
        println!("warning={warning}");
    }
    Ok(())
}

pub(super) fn print_acknowledgement(
    job_id: &str,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "jobId": job_id,
                "acknowledged": true
            }))?
        );
    } else {
        println!("job_id={job_id} acknowledged=true");
    }
    Ok(())
}

fn redacted_status(status: &MigrationStatus, secrets: &[String]) -> MigrationStatus {
    let mut redacted = status.clone();
    redacted.job_id = redact_secrets(&redacted.job_id, secrets);
    redacted.phase = redact_secrets(&redacted.phase, secrets);
    redacted.disposition = redact_secrets(&redacted.disposition, secrets);
    if let Some(target_index) = redacted.target_index.as_mut() {
        *target_index = redact_secrets(target_index, secrets);
    }
    if let Some(topology) = redacted.topology.as_mut() {
        *topology = redact_secrets(topology, secrets);
    }
    if let Some(created_at) = redacted.created_at.as_mut() {
        *created_at = redact_secrets(created_at, secrets);
    }
    if let Some(updated_at) = redacted.updated_at.as_mut() {
        *updated_at = redact_secrets(updated_at, secrets);
    }
    if let Some(terminal_at) = redacted.terminal_at.as_mut() {
        *terminal_at = redact_secrets(terminal_at, secrets);
    }
    for warning in &mut redacted.warnings {
        redact_json_value(warning, secrets);
    }
    redacted
}

fn redact_json_value(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(value) => *value = redact_secrets(value, secrets),
        Value::Array(values) => {
            for value in values {
                redact_json_value(value, secrets);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_json_value(value, secrets);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn append_count(fields: &mut Vec<String>, name: &str, count: Option<&MigrationCount>) {
    if let Some(count) = count {
        fields.push(format!("{name}={}", count.imported));
    }
}

fn redact_secrets(message: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(message.to_string(), |redacted, secret| {
            redacted.replace(secret, "[REDACTED]")
        })
}
