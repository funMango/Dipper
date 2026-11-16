//! `dipper ontology` reads a dictionary and writes local JSON and Markdown.

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::cli::OutputFormat;
use crate::error::DipperError;
use crate::ontology::{self, render, split_table, validate};
use crate::pipeline::{DefaultOpener, SourceOpener};

pub fn pull(
    config_path: &Path,
    source: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    pull_inner(
        config_path,
        source,
        table,
        log_sql,
        strict,
        format,
        &DefaultOpener,
        out,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn pull_with_opener(
    config_path: &Path,
    source: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    opener: &dyn SourceOpener,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    pull_inner(
        config_path,
        source,
        table,
        log_sql,
        strict,
        format,
        opener,
        out,
    )
}

#[allow(clippy::too_many_arguments)]
fn pull_inner(
    config_path: &Path,
    source: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    opener: &dyn SourceOpener,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    match ontology::pull(config_path, source, table, log_sql, strict, opener) {
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
                write_pull_json(out, source, &[], false, err.exit_code(), Some(&err))?;
            }
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn check_relation(
    config_path: &Path,
    source: &str,
    from_table: &str,
    from_columns: &[String],
    to_table: &str,
    to_columns: &[String],
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let outcome = run_check(
        config_path,
        source,
        from_table,
        from_columns,
        to_table,
        to_columns,
        log_sql,
        strict,
    );
    match outcome {
        Ok(body) => {
            if format == OutputFormat::Json {
                let text = serde_json::to_string(&body).map_err(write_json_err)?;
                writeln!(out, "{text}").map_err(write_err)?;
            } else {
                writeln!(out, "source_non_null_rows: {}", body.source_non_null_rows)
                    .map_err(write_err)?;
                writeln!(out, "target_non_null_rows: {}", body.target_non_null_rows)
                    .map_err(write_err)?;
                writeln!(out, "matched_rows: {}", body.matched_rows).map_err(write_err)?;
                writeln!(out, "match_ratio: {}", body.match_ratio).map_err(write_err)?;
            }
            Ok(())
        }
        Err(err) => {
            let secrets = crate::config::load(config_path)
                .map(|config| crate::output::oracle_secret_values(&config))
                .unwrap_or_default();
            let err = redact_ontology_error(err, &secrets);
            if format == OutputFormat::Json {
                let body = CheckDocument {
                    schema_version: 1,
                    command: "ontology.check_relation",
                    ok: false,
                    exit_code: err.exit_code(),
                    source: source.to_string(),
                    from: CheckSide {
                        owner: String::new(),
                        table: from_table.to_string(),
                        columns: from_columns.to_vec(),
                    },
                    to: CheckSide {
                        owner: String::new(),
                        table: to_table.to_string(),
                        columns: to_columns.to_vec(),
                    },
                    source_non_null_rows: 0,
                    target_non_null_rows: 0,
                    matched_rows: 0,
                    match_ratio: 0.0,
                    error: Some(crate::output::ErrorBody::from_error(&err)),
                };
                let text = serde_json::to_string(&body).map_err(write_json_err)?;
                writeln!(out, "{text}").map_err(write_err)?;
            }
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_check(
    config_path: &Path,
    source_name: &str,
    from_table: &str,
    from_columns: &[String],
    to_table: &str,
    to_columns: &[String],
    log_sql: bool,
    strict: bool,
) -> Result<CheckDocument, DipperError> {
    if from_columns.len() != to_columns.len() || from_columns.is_empty() {
        return Err(DipperError::Source {
            kind: crate::error::ErrorKind::Other,
            source_name: source_name.to_string(),
            message: "from-column and to-column counts must match".to_string(),
        });
    }
    let config = crate::config::load(config_path)?;
    let source_cfg = config
        .sources
        .get(source_name)
        .ok_or_else(|| DipperError::Source {
            kind: crate::error::ErrorKind::Other,
            source_name: source_name.to_string(),
            message: format!("unknown source: {source_name}"),
        })?;
    let (from_owner, from_name) = split_table(from_table, source_cfg)?;
    let (to_owner, to_name) = split_table(to_table, source_cfg)?;
    let strict = strict || config.strict_readonly;
    let opened = crate::source::open(
        source_name,
        source_cfg,
        &crate::source::OpenOptions {
            strict,
            log: if log_sql {
                crate::source::SqlLogSink::Stderr
            } else {
                crate::source::SqlLogSink::Off
            },
            action: format!("ontology:{source_name}"),
        },
    )?;
    let counts = opened.source.check_relation(
        &from_owner,
        &from_name,
        from_columns,
        &to_owner,
        &to_name,
        to_columns,
    )?;
    let ratio = if counts.source_non_null_rows == 0 {
        0.0
    } else {
        counts.matched_rows as f64 / counts.source_non_null_rows as f64
    };
    Ok(CheckDocument {
        schema_version: 1,
        command: "ontology.check_relation",
        ok: true,
        exit_code: 0,
        source: source_name.to_string(),
        from: CheckSide {
            owner: from_owner,
            table: from_name,
            columns: from_columns.to_vec(),
        },
        to: CheckSide {
            owner: to_owner,
            table: to_name,
            columns: to_columns.to_vec(),
        },
        source_non_null_rows: counts.source_non_null_rows,
        target_non_null_rows: counts.target_non_null_rows,
        matched_rows: counts.matched_rows,
        match_ratio: ratio,
        error: None,
    })
}

pub fn render_docs(root: &Path, out: &mut dyn Write) -> Result<(), DipperError> {
    let paths = render(root)?;
    for path in &paths {
        writeln!(out, "{}", path.display()).map_err(write_err)?;
    }
    writeln!(out, "{} ontology files rendered", paths.len()).map_err(write_err)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn list(
    config_path: &Path,
    source: &str,
    owner: Option<&str>,
    name_like: &str,
    limit: Option<u64>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let limit = page_limit(limit)?;
    let run = (|| {
        let config = crate::config::load(config_path)?;
        let source_cfg = config
            .sources
            .get(source)
            .ok_or_else(|| usage(format!("unknown source: {source}")))?;
        let owner = match (owner, source_cfg) {
            (Some(owner), _) => owner.to_string(),
            (None, crate::config::SourceConfig::Sqlite { .. }) => "main".to_string(),
            (None, crate::config::SourceConfig::Oracle { .. }) => {
                return Err(usage("ontology list requires --owner for an Oracle source"));
            }
        };
        let opened = open_source(
            source,
            source_cfg,
            log_sql,
            strict || config.strict_readonly,
        )?;
        opened.source.list_tables(&owner, name_like, limit)
    })();
    match run {
        Ok(page) => {
            if format == OutputFormat::Json {
                let tables: Vec<ListRow> = page
                    .items
                    .iter()
                    .map(|item| ListRow {
                        owner: item.owner.clone(),
                        table: item.table.clone(),
                        object_type: item.object_type.clone(),
                        comment: item.comment.clone(),
                        num_rows_estimate: item.num_rows_estimate,
                    })
                    .collect();
                write_doc(
                    out,
                    &ListDocument {
                        schema_version: 1,
                        command: "ontology.list",
                        ok: true,
                        exit_code: 0,
                        source,
                        owner: owner_label(config_path, source, owner),
                        truncated: page.truncated,
                        tables,
                        error: None,
                    },
                )?;
            } else {
                for item in &page.items {
                    writeln!(
                        out,
                        "{}.{} {} {}",
                        item.owner,
                        item.table,
                        item.object_type,
                        item.comment.as_deref().unwrap_or("")
                    )
                    .map_err(write_err)?;
                }
                if page.truncated {
                    writeln!(out, "truncated").map_err(write_err)?;
                }
            }
            Ok(())
        }
        Err(err) => {
            let err = redact_loaded(config_path, err);
            if format == OutputFormat::Json {
                write_doc(
                    out,
                    &ListDocument {
                        schema_version: 1,
                        command: "ontology.list",
                        ok: false,
                        exit_code: err.exit_code(),
                        source,
                        owner: owner.unwrap_or("").to_string(),
                        truncated: false,
                        tables: Vec::new(),
                        error: Some(crate::output::ErrorBody::from_error(&err)),
                    },
                )?;
            }
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn find(
    config_path: &Path,
    source: &str,
    column: &str,
    owner: Option<&str>,
    limit: Option<u64>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let limit = page_limit(limit)?;
    let run = (|| {
        let config = crate::config::load(config_path)?;
        let source_cfg = config
            .sources
            .get(source)
            .ok_or_else(|| usage(format!("unknown source: {source}")))?;
        let opened = open_source(
            source,
            source_cfg,
            log_sql,
            strict || config.strict_readonly,
        )?;
        opened.source.find_columns(column, owner, limit)
    })();
    match run {
        Ok(page) => {
            if format == OutputFormat::Json {
                let hits: Vec<FindRow> = page
                    .items
                    .iter()
                    .map(|item| FindRow {
                        owner: item.owner.clone(),
                        table: item.table.clone(),
                        data_type: item.data_type.clone(),
                        data_length: item.data_length,
                        data_precision: item.data_precision,
                        data_scale: item.data_scale,
                        nullable: item.nullable,
                    })
                    .collect();
                write_doc(
                    out,
                    &FindDocument {
                        schema_version: 1,
                        command: "ontology.find",
                        ok: true,
                        exit_code: 0,
                        source,
                        column,
                        truncated: page.truncated,
                        hits,
                        error: None,
                    },
                )?;
            } else {
                for item in &page.items {
                    writeln!(
                        out,
                        "{}.{} {} {}",
                        item.owner, item.table, item.data_type, item.nullable
                    )
                    .map_err(write_err)?;
                }
                if page.truncated {
                    writeln!(out, "truncated").map_err(write_err)?;
                }
            }
            Ok(())
        }
        Err(err) => {
            let err = redact_loaded(config_path, err);
            if format == OutputFormat::Json {
                write_doc(
                    out,
                    &FindDocument {
                        schema_version: 1,
                        command: "ontology.find",
                        ok: false,
                        exit_code: err.exit_code(),
                        source,
                        column,
                        truncated: false,
                        hits: Vec::new(),
                        error: Some(crate::output::ErrorBody::from_error(&err)),
                    },
                )?;
            }
            Err(err)
        }
    }
}

fn page_limit(limit: Option<u64>) -> Result<usize, DipperError> {
    let limit = limit.unwrap_or(200);
    if limit == 0 || limit > 2000 {
        return Err(usage("limit must be from 1 to 2000"));
    }
    Ok(limit as usize)
}

fn usage(message: impl Into<String>) -> DipperError {
    DipperError::Source {
        source_name: "ontology".to_string(),
        kind: crate::error::ErrorKind::Usage,
        message: message.into(),
    }
}

fn open_source(
    name: &str,
    cfg: &crate::config::SourceConfig,
    log_sql: bool,
    strict: bool,
) -> Result<crate::source::Opened, DipperError> {
    crate::source::open(
        name,
        cfg,
        &crate::source::OpenOptions {
            strict,
            log: if log_sql {
                crate::source::SqlLogSink::Stderr
            } else {
                crate::source::SqlLogSink::Off
            },
            action: format!("ontology:{name}"),
        },
    )
}

fn owner_label(config_path: &Path, source: &str, owner: Option<&str>) -> String {
    if let Some(owner) = owner {
        return owner.to_string();
    }
    let Ok(config) = crate::config::load(config_path) else {
        return String::new();
    };
    match config.sources.get(source) {
        Some(crate::config::SourceConfig::Sqlite { .. }) => "main".to_string(),
        _ => String::new(),
    }
}

fn redact_loaded(config_path: &Path, err: DipperError) -> DipperError {
    let secrets = crate::config::load(config_path)
        .map(|config| crate::output::oracle_secret_values(&config))
        .unwrap_or_default();
    redact_ontology_error(err, &secrets)
}

fn write_doc(out: &mut dyn Write, doc: &impl Serialize) -> Result<(), DipperError> {
    let text = serde_json::to_string(doc).map_err(write_json_err)?;
    writeln!(out, "{text}").map_err(write_err)
}

pub fn validate_docs(root: &Path, out: &mut dyn Write) -> Result<(), DipperError> {
    validate(root)?;
    writeln!(out, "ontology ok").map_err(write_err)?;
    Ok(())
}

#[derive(Serialize)]
struct ListRow {
    owner: String,
    table: String,
    object_type: String,
    comment: Option<String>,
    num_rows_estimate: Option<i64>,
}

#[derive(Serialize)]
struct ListDocument<'a> {
    schema_version: i32,
    command: &'static str,
    ok: bool,
    exit_code: i32,
    source: &'a str,
    owner: String,
    truncated: bool,
    tables: Vec<ListRow>,
    error: Option<crate::output::ErrorBody>,
}

#[derive(Serialize)]
struct FindRow {
    owner: String,
    table: String,
    data_type: String,
    data_length: Option<i64>,
    data_precision: Option<i64>,
    data_scale: Option<i64>,
    nullable: bool,
}

#[derive(Serialize)]
struct FindDocument<'a> {
    schema_version: i32,
    command: &'static str,
    ok: bool,
    exit_code: i32,
    source: &'a str,
    column: &'a str,
    truncated: bool,
    hits: Vec<FindRow>,
    error: Option<crate::output::ErrorBody>,
}

#[derive(Serialize)]
struct PullDocument<'a> {
    schema_version: i32,
    command: &'static str,
    ok: bool,
    exit_code: i32,
    source: &'a str,
    files: &'a [String],
    error: Option<crate::output::ErrorBody>,
}

#[derive(Serialize)]
struct CheckSide {
    owner: String,
    table: String,
    columns: Vec<String>,
}

#[derive(Serialize)]
struct CheckDocument {
    schema_version: i32,
    command: &'static str,
    ok: bool,
    exit_code: i32,
    source: String,
    from: CheckSide,
    to: CheckSide,
    source_non_null_rows: u64,
    target_non_null_rows: u64,
    matched_rows: u64,
    match_ratio: f64,
    error: Option<crate::output::ErrorBody>,
}

fn write_pull_json(
    out: &mut dyn Write,
    source: &str,
    files: &[String],
    ok: bool,
    exit_code: i32,
    error: Option<&DipperError>,
) -> Result<(), DipperError> {
    let doc = PullDocument {
        schema_version: 1,
        command: "ontology.pull",
        ok,
        exit_code,
        source,
        files,
        error: error.map(crate::output::ErrorBody::from_error),
    };
    let text = serde_json::to_string(&doc).map_err(write_json_err)?;
    writeln!(out, "{text}").map_err(write_err)
}

fn redact_ontology_error(
    error: DipperError,
    secrets: &[crate::output::KnownSecret],
) -> DipperError {
    crate::output::redact_error(error, secrets)
}

fn write_err(err: std::io::Error) -> DipperError {
    DipperError::Source {
        kind: crate::error::ErrorKind::Other,
        source_name: "stdout".to_string(),
        message: err.to_string(),
    }
}

fn write_json_err(err: serde_json::Error) -> DipperError {
    DipperError::Source {
        kind: crate::error::ErrorKind::Other,
        source_name: "stdout".to_string(),
        message: err.to_string(),
    }
}
