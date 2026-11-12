//! `dipper ontology` reads a dictionary and writes local JSON and Markdown.

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::cli::OutputFormat;
use crate::error::DipperError;
use crate::ontology::{self, render, validate};

pub fn pull(
    config_path: &Path,
    source: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    match ontology::pull(config_path, source, table, log_sql, strict) {
        Ok(paths) => {
            let files: Vec<String> = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            if format == OutputFormat::Json {
                write_pull_json(out, source, &files, true, 0, None)?;
            } else {
                for file in &files {
                    writeln!(out, "{file}").map_err(write_err)?;
                }
            }
            Ok(())
        }
        Err(err) => {
            let secrets = crate::config::load(config_path)
                .map(|config| crate::output::oracle_secret_values(&config))
                .unwrap_or_default();
            let err = redact_ontology_error(err, &secrets);
            if format == OutputFormat::Json {
                write_pull_json(
                    out,
                    source,
                    &[],
                    false,
                    err.exit_code(),
                    Some(err.to_string()),
                )?;
            }
            Err(err)
        }
    }
}

pub fn render_docs(root: &Path, out: &mut dyn Write) -> Result<(), DipperError> {
    let paths = render(root)?;
    for path in &paths {
        writeln!(out, "{}", path.display()).map_err(write_err)?;
    }
    writeln!(out, "{} ontology files rendered", paths.len()).map_err(write_err)?;
    Ok(())
}

pub fn validate_docs(root: &Path, out: &mut dyn Write) -> Result<(), DipperError> {
    validate(root)?;
    writeln!(out, "ontology ok").map_err(write_err)?;
    Ok(())
}

#[derive(Serialize)]
struct PullDocument<'a> {
    schema_version: i32,
    command: &'static str,
    ok: bool,
    exit_code: i32,
    source: &'a str,
    files: &'a [String],
    error: Option<PullError>,
}

#[derive(Serialize)]
struct PullError {
    message: String,
}

fn write_pull_json(
    out: &mut dyn Write,
    source: &str,
    files: &[String],
    ok: bool,
    exit_code: i32,
    message: Option<String>,
) -> Result<(), DipperError> {
    let doc = PullDocument {
        schema_version: 1,
        command: "ontology.pull",
        ok,
        exit_code,
        source,
        files,
        error: message.map(|message| PullError { message }),
    };
    let text = serde_json::to_string(&doc).map_err(write_json_err)?;
    writeln!(out, "{text}").map_err(write_err)
}

fn redact_ontology_error(error: DipperError, secrets: &[String]) -> DipperError {
    if secrets.is_empty() {
        return error;
    }
    let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
    match error {
        DipperError::Source {
            source_name,
            message,
        } => DipperError::Source {
            source_name,
            message: crate::output::redact_known_values(&message, &refs),
        },
        other => other,
    }
}

fn write_err(err: std::io::Error) -> DipperError {
    DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    }
}

fn write_json_err(err: serde_json::Error) -> DipperError {
    DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    }
}
