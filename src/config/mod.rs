mod env;
mod period;
mod schema;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use env::{substitute, substitute_secret};
pub use period::{
    apply_period, describe_window, ledger_interval, parse_instant, range_exceeds_limit,
    resolve_period, rows_exceed_preview, AppliedPeriod, PeriodInstant, PeriodWindow,
    MAX_PERIOD_DAYS, MAX_PREVIEW_ROWS,
};
pub use schema::{Config, Dip, Filter, FilterOp, Paging, Performance, Secret, SourceConfig};

use crate::error::{ConfigIssue, DipperError};
use crate::sql::{validate_column_ident, validate_dip_name, validate_ident};
use schema::{
    RawConfig, RawPerformance, RawSource, DEFAULT_BATCH_ROWS, DEFAULT_CALL_TIMEOUT_SECS,
    DEFAULT_MAX_SESSION_MINUTES, DEFAULT_MEMORY_LIMIT, DEFAULT_PROGRESS_EVERY_ROWS, DEFAULT_STORE,
    DEFAULT_TEMP_DIRECTORY, PASSWORD_MUST_BE_STRING,
};

/// Load `path`, substituting environment variables from the process environment.
pub fn load(path: &Path) -> Result<Config, DipperError> {
    load_with_env(path, &|name| std::env::var(name).ok())
}

/// Load `path`, substituting `${NAME}` through `lookup`.
pub fn load_with_env(
    path: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Config, DipperError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(DipperError::ConfigNotFound(path.to_path_buf()));
        }
        Err(source) => {
            return Err(DipperError::ConfigRead {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let raw = parse_raw(path, &text)?;
    let mut issues = Vec::new();
    let performance = validate(&raw, &mut issues);
    if !issues.is_empty() {
        return Err(invalid(path, issues));
    }

    let base = config_dir(path);
    let mut sub_issues = Vec::new();
    let sources = substitute_sources(&raw, lookup, &base, &mut sub_issues);
    if !sub_issues.is_empty() {
        return Err(invalid(path, sub_issues));
    }

    let store_value = raw.store.clone();
    let dips = raw
        .dips
        .iter()
        .map(|dip| Dip {
            name: dip.name.clone(),
            source: dip.source.clone(),
            table: dip.table.clone(),
            columns: dip.columns.clone(),
            filters: dip
                .filters
                .iter()
                .map(|filter| Filter {
                    column: filter.column.clone(),
                    op: filter.op,
                    value: filter.value.clone(),
                })
                .collect(),
            limit: dip.limit,
            count_first: dip.count_first.unwrap_or(false),
            key: dip.key.clone(),
            paging: dip.paging.unwrap_or(schema::Paging::Keyset),
        })
        .collect();

    Ok(Config {
        file: path.to_path_buf(),
        strict_readonly: raw
            .safety
            .as_ref()
            .and_then(|safety| safety.strict_readonly)
            .unwrap_or(false),
        store: resolve_path(&base, store_value.as_deref().unwrap_or(DEFAULT_STORE)),
        performance: Performance {
            batch_rows: performance.batch_rows,
            memory_limit: performance.memory_limit,
            temp_directory: resolve_path(&base, &performance.temp_directory),
            progress_every_rows: performance.progress_every_rows,
        },
        sources,
        dips,
    })
}

/// Parse a memory limit into a byte count.
///
/// Accepts a number (fractional part allowed), optional whitespace, and a unit
/// `KB|MB|GB|TB|KiB|MiB|GiB|TiB` (case-insensitive). Decimal units are powers of
/// 1000 and binary units are powers of 1024. The result must be greater than zero.
pub fn parse_memory_limit(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() || s.starts_with('-') || s.starts_with('+') {
        return Err("invalid memory_limit".to_string());
    }

    let bytes = s.as_bytes();
    let mut end = 0;
    let mut dot = false;
    while end < bytes.len() {
        match bytes[end] {
            b'0'..=b'9' => end += 1,
            b'.' if !dot => {
                dot = true;
                end += 1;
            }
            _ => break,
        }
    }
    let number = &s[..end];
    if number.is_empty()
        || number == "."
        || number.starts_with('.')
        || number.ends_with('.')
        || number.matches('.').count() > 1
    {
        return Err("invalid memory_limit".to_string());
    }
    let number: f64 = number
        .parse()
        .map_err(|_| "invalid memory_limit".to_string())?;
    if !number.is_finite() || number <= 0.0 {
        return Err("invalid memory_limit".to_string());
    }

    let unit = s[end..].trim_start();
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err("invalid memory_limit".to_string()),
    };

    let bytes = number * multiplier;
    if !bytes.is_finite() || bytes <= 0.0 || bytes > u64::MAX as f64 {
        return Err("invalid memory_limit".to_string());
    }
    let rounded = bytes.round();
    if rounded < 1.0 || rounded > u64::MAX as f64 {
        return Err("invalid memory_limit".to_string());
    }
    Ok(rounded as u64)
}

struct CheckedPerformance {
    batch_rows: usize,
    memory_limit: String,
    temp_directory: String,
    progress_every_rows: u64,
}

fn parse_raw(path: &Path, text: &str) -> Result<RawConfig, DipperError> {
    let de = match toml::Deserializer::parse(text) {
        Ok(de) => de,
        Err(err) => return Err(invalid(path, vec![toml_issue(text, "", &err)])),
    };
    match serde_path_to_error::deserialize(de) {
        Ok(raw) => Ok(raw),
        Err(err) => {
            let field_path = err.path().to_string();
            Err(invalid(
                path,
                vec![toml_issue(text, &field_path, err.inner())],
            ))
        }
    }
}

fn toml_issue(text: &str, path: &str, err: &toml::de::Error) -> ConfigIssue {
    let path = if path == "." {
        String::new()
    } else {
        path.to_string()
    };
    let message = err.message().to_string();
    // Internally tagged source tables report this type error on the source,
    // not on the password field. The message never contains the rejected value.
    let path = if message == PASSWORD_MUST_BE_STRING && !path.ends_with(".password") {
        if path.is_empty() {
            "password".to_string()
        } else {
            format!("{path}.password")
        }
    } else {
        path
    };
    ConfigIssue {
        path,
        line_col: err.span().map(|span| offset_to_line_col(text, span.start)),
        message,
    }
}

fn offset_to_line_col(text: &str, index: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return (1, 1);
    }
    let index = index.min(bytes.len());
    let safe = index.min(bytes.len() - 1);
    let column_offset = index - safe;
    let line_start = bytes[..safe]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |nl| nl + 1);
    let line = bytes[..line_start]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1;
    let column = text
        .get(line_start..=safe)
        .map_or(1, |slice| slice.chars().count())
        + column_offset;
    (line, column)
}

fn validate(raw: &RawConfig, issues: &mut Vec<ConfigIssue>) -> CheckedPerformance {
    let performance = validate_performance(raw.performance.as_ref(), issues);
    validate_sources(raw, issues);
    if raw.sources.is_empty() {
        push(issues, "sources", "at least one source is required");
    }
    let mut seen_names: Vec<String> = Vec::new();
    for (index, dip) in raw.dips.iter().enumerate() {
        validate_dip(index, dip, &raw.sources, &mut seen_names, issues);
    }
    performance
}

fn validate_performance(
    performance: Option<&RawPerformance>,
    issues: &mut Vec<ConfigIssue>,
) -> CheckedPerformance {
    let batch_rows = match performance.and_then(|item| item.batch_rows) {
        Some(0) => {
            push(issues, "performance.batch_rows", "must be greater than 0");
            DEFAULT_BATCH_ROWS
        }
        Some(value) => match usize::try_from(value) {
            Ok(value) => value,
            Err(_) => {
                push(issues, "performance.batch_rows", "must fit in usize");
                DEFAULT_BATCH_ROWS
            }
        },
        None => DEFAULT_BATCH_ROWS,
    };

    let memory_limit = match performance.and_then(|item| item.memory_limit.clone()) {
        Some(value) => {
            if let Err(message) = parse_memory_limit(&value) {
                push(issues, "performance.memory_limit", message);
            }
            value
        }
        None => DEFAULT_MEMORY_LIMIT.to_string(),
    };

    let temp_directory = performance
        .and_then(|item| item.temp_directory.clone())
        .unwrap_or_else(|| DEFAULT_TEMP_DIRECTORY.to_string());

    let progress_every_rows = match performance.and_then(|item| item.progress_every_rows) {
        Some(0) => {
            push(
                issues,
                "performance.progress_every_rows",
                "must be greater than 0",
            );
            DEFAULT_PROGRESS_EVERY_ROWS
        }
        Some(value) => value,
        None => DEFAULT_PROGRESS_EVERY_ROWS,
    };

    CheckedPerformance {
        batch_rows,
        memory_limit,
        temp_directory,
        progress_every_rows,
    }
}

fn validate_dip(
    index: usize,
    dip: &schema::RawDip,
    sources: &BTreeMap<String, RawSource>,
    seen_names: &mut Vec<String>,
    issues: &mut Vec<ConfigIssue>,
) {
    if let Err(message) = validate_dip_name(&dip.name) {
        push(issues, format!("dips[{index}].name"), message);
    }
    if seen_names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&dip.name))
    {
        push(
            issues,
            format!("dips[{index}].name"),
            format!("duplicate dip name '{}'", dip.name),
        );
    }
    seen_names.push(dip.name.clone());

    if !sources.contains_key(&dip.source) {
        push(
            issues,
            format!("dips[{index}].source"),
            format!("unknown source '{}'", dip.source),
        );
    }

    if let Err(message) = validate_ident(&dip.table) {
        push(issues, format!("dips[{index}].table"), message);
    }

    if dip.columns.is_empty() {
        push(
            issues,
            format!("dips[{index}].columns"),
            "at least one column is required",
        );
    }
    let mut seen_columns: Vec<&str> = Vec::new();
    for (column_index, column) in dip.columns.iter().enumerate() {
        let path = format!("dips[{index}].columns[{column_index}]");
        if let Err(message) = validate_column_ident(column) {
            push(issues, path.clone(), message);
        }
        if seen_columns.iter().any(|seen| *seen == column) {
            push(issues, path, "duplicate column");
        }
        seen_columns.push(column);
    }

    for (filter_index, filter) in dip.filters.iter().enumerate() {
        if let Err(message) = validate_column_ident(&filter.column) {
            push(
                issues,
                format!("dips[{index}].filters[{filter_index}].column"),
                message,
            );
        }
        if let Err(message) = validate_filter_value(filter.op, filter.value.as_ref()) {
            push(
                issues,
                format!("dips[{index}].filters[{filter_index}].value"),
                message,
            );
        }
    }

    if dip.limit == Some(0) {
        push(
            issues,
            format!("dips[{index}].limit"),
            "must be greater than 0",
        );
    }
    if let Some(key) = &dip.key {
        if let Err(message) = validate_column_ident(key) {
            push(issues, format!("dips[{index}].key"), message);
        }
    }
}

fn validate_sources(raw: &RawConfig, issues: &mut Vec<ConfigIssue>) {
    for (name, source) in &raw.sources {
        match source {
            RawSource::Sqlite {
                max_session_minutes,
                ..
            } => {
                require_positive(
                    issues,
                    format!("sources.{name}.max_session_minutes"),
                    *max_session_minutes,
                );
            }
            RawSource::Oracle {
                call_timeout_secs,
                max_session_minutes,
                ..
            } => {
                require_positive(
                    issues,
                    format!("sources.{name}.call_timeout_secs"),
                    *call_timeout_secs,
                );
                require_positive(
                    issues,
                    format!("sources.{name}.max_session_minutes"),
                    *max_session_minutes,
                );
            }
        }
    }
}

fn require_positive(issues: &mut Vec<ConfigIssue>, path: String, value: Option<u64>) {
    if value == Some(0) {
        push(issues, path, "must be greater than 0");
    }
}

fn validate_filter_value(op: FilterOp, value: Option<&toml::Value>) -> Result<(), String> {
    match op {
        FilterOp::IsNull | FilterOp::NotNull => {
            if value.is_some() {
                Err(format!("{} does not take a value", op_name(op)))
            } else {
                Ok(())
            }
        }
        FilterOp::In => match value {
            Some(toml::Value::Array(items)) if !items.is_empty() => Ok(()),
            _ => Err("in requires a non-empty array".to_string()),
        },
        FilterOp::Between => match value {
            Some(toml::Value::Array(items)) if items.len() == 2 => Ok(()),
            _ => Err("between requires an array of two values".to_string()),
        },
        FilterOp::Eq
        | FilterOp::Ne
        | FilterOp::Gt
        | FilterOp::Gte
        | FilterOp::Lt
        | FilterOp::Lte
        | FilterOp::Like => match value {
            Some(toml::Value::Array(_) | toml::Value::Table(_)) | None => {
                Err("a single value is required".to_string())
            }
            Some(_) => Ok(()),
        },
    }
}

fn op_name(op: FilterOp) -> &'static str {
    match op {
        FilterOp::Eq => "eq",
        FilterOp::Ne => "ne",
        FilterOp::In => "in",
        FilterOp::Gt => "gt",
        FilterOp::Gte => "gte",
        FilterOp::Lt => "lt",
        FilterOp::Lte => "lte",
        FilterOp::Between => "between",
        FilterOp::Like => "like",
        FilterOp::IsNull => "is_null",
        FilterOp::NotNull => "not_null",
    }
}

fn substitute_sources(
    raw: &RawConfig,
    lookup: &dyn Fn(&str) -> Option<String>,
    base: &Path,
    issues: &mut Vec<ConfigIssue>,
) -> BTreeMap<String, SourceConfig> {
    let mut sources = BTreeMap::new();
    for (name, source) in &raw.sources {
        match source {
            RawSource::Sqlite {
                path,
                max_session_minutes,
            } => {
                let field = format!("sources.{name}.path");
                match substitute(path, &field, lookup) {
                    Ok(value) => {
                        sources.insert(
                            name.clone(),
                            SourceConfig::Sqlite {
                                path: resolve_path(base, &value),
                                max_session_minutes: max_session_minutes
                                    .unwrap_or(DEFAULT_MAX_SESSION_MINUTES),
                            },
                        );
                    }
                    Err(issue) => issues.push(issue),
                }
            }
            RawSource::Oracle {
                connect,
                user,
                password,
                call_timeout_secs,
                max_session_minutes,
            } => {
                let connect_field = format!("sources.{name}.connect");
                let user_field = format!("sources.{name}.user");
                let password_field = format!("sources.{name}.password");
                let connect = substitute(connect, &connect_field, lookup);
                let user = substitute(user, &user_field, lookup);
                let password = substitute_secret(password.as_str(), &password_field, lookup);
                match (connect, user, password) {
                    (Ok(connect), Ok(user), Ok(password)) => {
                        sources.insert(
                            name.clone(),
                            SourceConfig::Oracle {
                                connect,
                                user,
                                password,
                                call_timeout_secs: call_timeout_secs
                                    .unwrap_or(DEFAULT_CALL_TIMEOUT_SECS),
                                max_session_minutes: max_session_minutes
                                    .unwrap_or(DEFAULT_MAX_SESSION_MINUTES),
                            },
                        );
                    }
                    (connect, user, password) => {
                        if let Err(issue) = connect {
                            issues.push(issue);
                        }
                        if let Err(issue) = user {
                            issues.push(issue);
                        }
                        if let Err(issue) = password {
                            issues.push(issue);
                        }
                    }
                }
            }
        }
    }
    sources
}

fn resolve_path(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn config_dir(path: &Path) -> PathBuf {
    path.parent().unwrap_or_else(|| Path::new("")).to_path_buf()
}

fn invalid(path: &Path, issues: Vec<ConfigIssue>) -> DipperError {
    DipperError::Config {
        file: path.to_path_buf(),
        issues,
    }
}

fn push(issues: &mut Vec<ConfigIssue>, path: impl Into<String>, message: impl Into<String>) {
    issues.push(ConfigIssue {
        path: path.into(),
        line_col: None,
        message: message.into(),
    });
}

#[cfg(test)]
mod password_tests {
    use super::*;

    #[test]
    fn substituted_password_is_readable_inside_the_crate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dipper.toml");
        std::fs::write(
            &path,
            r#"
[sources.company]
kind = "oracle"
connect = "host/${DIPPER_TEST_CONNECT}"
user = "${DIPPER_TEST_ORA_USER}"
password = "${DIPPER_TEST_ORA_PASSWORD}"
"#,
        )
        .unwrap();
        let config = load_with_env(&path, &|key| match key {
            "DIPPER_TEST_CONNECT" => Some("db.example:1521/SVC".to_string()),
            "DIPPER_TEST_ORA_USER" => Some("scott".to_string()),
            "DIPPER_TEST_ORA_PASSWORD" => Some("PR2-loader-secret".to_string()),
            _ => None,
        })
        .unwrap();
        match config.sources.get("company") {
            Some(SourceConfig::Oracle {
                connect,
                user,
                password,
                ..
            }) => {
                assert_eq!(connect, "host/db.example:1521/SVC");
                assert_eq!(user, "scott");
                assert_eq!(password.expose(), "PR2-loader-secret");
            }
            other => panic!("company: {other:?}"),
        }
    }
}
