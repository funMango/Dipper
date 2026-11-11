//! `dipper ontology` reads a dictionary and writes local JSON and Markdown.

use std::io::Write;
use std::path::Path;

use crate::error::DipperError;
use crate::ontology::{self, render, validate};

pub fn pull(
    config_path: &Path,
    source: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let paths = ontology::pull(config_path, source, table, log_sql, strict)?;
    for path in paths {
        writeln!(out, "{}", path.display()).map_err(write_err)?;
    }
    Ok(())
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

fn write_err(err: std::io::Error) -> DipperError {
    DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    }
}
