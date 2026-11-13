mod logging;
mod preflight;
mod session;
mod sqlite;
mod watchdog;

#[cfg(feature = "oracle")]
mod oracle;

use std::time::Duration;

use crate::config::{Config, Filter, SourceConfig};
use crate::error::DipperError;

pub use logging::SqlLogSink;
pub use preflight::{
    parse_dict, DictQuery, Finding, KeySource, Level, PreflightReport, ResolvedKey,
};
pub use session::SessionStatement;
pub use sqlite::SqliteSource;

/// How a dip walks the source table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paging {
    Keyset,
    Snapshot,
}

/// What to read from a source. SQL is built from these fields, not supplied by the caller.
#[derive(Debug)]
pub struct SelectSpec {
    pub table: String,
    pub columns: Vec<String>,
    pub filters: Vec<Filter>,
    pub limit: Option<u64>,
    pub batch_rows: usize,
    pub key: Option<String>,
    pub paging: Paging,
}

/// A scalar value read from a source. Dates and timestamps arrive in PR3.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Date(chrono::NaiveDate),
    Timestamp(chrono::NaiveDateTime),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

pub type ChunkIter<'a> = Box<dyn Iterator<Item = Result<Chunk, DipperError>> + 'a>;

/// One column read from a data dictionary. Meaning is not filled here.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogColumn {
    pub name: String,
    pub data_type: String,
    pub data_length: Option<i64>,
    pub data_precision: Option<i64>,
    pub data_scale: Option<i64>,
    pub nullable: bool,
    pub virtual_column: bool,
    pub db_comment: Option<String>,
}

/// One declared foreign key. Column order follows the dictionary position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogForeignKey {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub target_owner: String,
    pub target_table: String,
    pub target_columns: Vec<String>,
}

/// Table or view shape read from a data dictionary.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogTable {
    pub object_type: String,
    pub table_comment: Option<String>,
    pub primary_key: Vec<String>,
    pub columns: Vec<CatalogColumn>,
    pub foreign_keys: Vec<CatalogForeignKey>,
}

/// Aggregate overlap counts. No row values are included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlapCounts {
    pub source_non_null_rows: u64,
    pub target_non_null_rows: u64,
    pub matched_rows: u64,
}

pub trait Source {
    fn name(&self) -> &str;
    fn kind(&self) -> &'static str;
    fn test_connection(&self) -> Result<(), DipperError>;
    fn preflight(&self, dip: &str, spec: &SelectSpec) -> Result<PreflightReport, DipperError>;
    fn count(&self, spec: &SelectSpec) -> Result<u64, DipperError>;
    fn select<'a>(&'a self, spec: &SelectSpec) -> Result<ChunkIter<'a>, DipperError>;
    /// Read column, key, and comment metadata. This never reads table rows.
    fn catalog(&self, owner: &str, table: &str) -> Result<CatalogTable, DipperError>;
    /// Count how source column values overlap a target table. Counts only.
    fn check_relation(
        &self,
        from_owner: &str,
        from_table: &str,
        from_columns: &[String],
        to_owner: &str,
        to_table: &str,
        to_columns: &[String],
    ) -> Result<OverlapCounts, DipperError> {
        let _ = (
            from_owner,
            from_table,
            from_columns,
            to_owner,
            to_table,
            to_columns,
        );
        Err(DipperError::Source {
            source_name: self.name().to_string(),
            message: "check-relation is not supported".to_string(),
        })
    }
}

pub(crate) fn cell_text(row: &[Value], index: usize) -> Option<String> {
    match row.get(index) {
        Some(Value::Text(text)) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        }
        Some(Value::Int(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn cell_i64(row: &[Value], index: usize) -> Result<Option<i64>, String> {
    match row.get(index) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Int(value)) => Ok(Some(*value)),
        Some(Value::Float(value))
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value <= i64::MAX as f64 =>
        {
            Ok(Some(*value as i64))
        }
        Some(Value::Text(text)) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(None);
            }
            if text.contains('.') || text.contains('e') || text.contains('E') {
                let number: f64 = text
                    .parse()
                    .map_err(|_| format!("unexpected number {text}"))?;
                if number.is_finite() && number.fract() == 0.0 {
                    return Ok(Some(number as i64));
                }
                return Err(format!("unexpected number {text}"));
            }
            text.parse::<i64>()
                .map(Some)
                .map_err(|_| format!("unexpected number {text}"))
        }
        Some(other) => Err(format!("unexpected number {other:?}")),
    }
}

/// How `open` should treat warnings and statement logging.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub strict: bool,
    pub log: SqlLogSink,
    pub action: String,
}

/// A source plus the notes and warnings from session setup.
pub struct Opened {
    pub source: Box<dyn Source>,
    pub findings: Vec<Finding>,
}

#[derive(Debug)]
pub struct CheckResult {
    pub name: String,
    pub kind: &'static str,
    pub outcome: Result<(), String>,
    pub findings: Vec<Finding>,
}

pub fn open(name: &str, cfg: &SourceConfig, opts: &OpenOptions) -> Result<Opened, DipperError> {
    match cfg {
        SourceConfig::Sqlite {
            path,
            max_session_minutes,
        } => SqliteSource::open(name, path, *max_session_minutes, opts),
        SourceConfig::Oracle {
            connect,
            user,
            password,
            call_timeout_secs,
            max_session_minutes,
        } => open_oracle(
            name,
            connect,
            user,
            password,
            *call_timeout_secs,
            *max_session_minutes,
            opts,
        ),
    }
}

#[cfg(feature = "oracle")]
fn open_oracle(
    name: &str,
    connect: &str,
    user: &str,
    password: &crate::config::Secret,
    call_timeout_secs: u64,
    max_session_minutes: u64,
    opts: &OpenOptions,
) -> Result<Opened, DipperError> {
    oracle::OracleSource::connect(
        name,
        connect,
        user,
        password,
        call_timeout_secs,
        max_session_minutes,
        opts,
    )
}

#[cfg(not(feature = "oracle"))]
fn open_oracle(
    name: &str,
    connect: &str,
    user: &str,
    password: &crate::config::Secret,
    call_timeout_secs: u64,
    max_session_minutes: u64,
    opts: &OpenOptions,
) -> Result<Opened, DipperError> {
    let _ = (
        connect,
        user,
        password,
        call_timeout_secs,
        max_session_minutes,
        opts,
    );
    Err(DipperError::Source {
        source_name: name.to_string(),
        message: "oracle support is not built in (build with --features oracle)".to_string(),
    })
}

/// Open every source and test it. One failure does not stop the rest.
/// Results follow source name order.
pub fn check_all(config: &Config, opts: &OpenOptions) -> Vec<CheckResult> {
    config
        .sources
        .iter()
        .map(|(name, cfg)| match open(name, cfg, opts) {
            Ok(opened) => {
                let mut findings = opened.findings;
                let mut outcome = opened.source.test_connection().map_err(failure_reason);
                if outcome.is_ok() {
                    for dip in config.dips.iter().filter(|dip| &dip.source == name) {
                        match opened.source.preflight(&dip.name, &spec_for(config, dip)) {
                            Ok(report) => findings.extend(report.findings),
                            Err(err) => {
                                outcome = Err(failure_reason(err));
                                break;
                            }
                        }
                    }
                }
                if outcome.is_ok() {
                    outcome = summarize(&findings, opts.strict);
                }
                CheckResult {
                    name: name.clone(),
                    kind: opened.source.kind(),
                    outcome,
                    findings,
                }
            }
            Err(err) => CheckResult {
                name: name.clone(),
                kind: kind_of(cfg),
                outcome: Err(failure_reason(err)),
                findings: Vec::new(),
            },
        })
        .collect()
}

fn spec_for(config: &Config, dip: &crate::config::Dip) -> SelectSpec {
    SelectSpec {
        table: dip.table.clone(),
        columns: dip.columns.clone(),
        filters: dip.filters.clone(),
        limit: dip.limit,
        batch_rows: config.performance.batch_rows,
        key: dip.key.clone(),
        paging: match dip.paging {
            crate::config::Paging::Keyset => Paging::Keyset,
            crate::config::Paging::Snapshot => Paging::Snapshot,
        },
    }
}

fn summarize(findings: &[Finding], strict: bool) -> Result<(), String> {
    if let Some(finding) = findings
        .iter()
        .find(|finding| finding.level == Level::Refuse)
    {
        let dip = finding.dip.as_deref().unwrap_or("");
        return Err(format!("refused: {dip}: {}", finding.message));
    }
    if strict {
        let warnings = findings
            .iter()
            .filter(|finding| finding.level == Level::Warn)
            .count();
        if warnings > 0 {
            return Err(format!("strict: {warnings} warning(s)"));
        }
    }
    Ok(())
}

pub(crate) fn session_deadline(minutes: u64) -> Duration {
    Duration::from_secs(minutes.saturating_mul(60))
}

fn kind_of(cfg: &SourceConfig) -> &'static str {
    match cfg {
        SourceConfig::Sqlite { .. } => "sqlite",
        SourceConfig::Oracle { .. } => "oracle",
    }
}

fn failure_reason(err: DipperError) -> String {
    match err {
        DipperError::Source { message, .. } => message,
        other => other.to_string(),
    }
}

/// Hide a driver error that contains the password.
///
/// A short password can be a substring of a normal Oracle code (`ORA`).
/// Replacing that substring would show where the secret sat in the text, so
/// the whole message is dropped and only the first driver code is kept.
#[cfg_attr(not(any(test, feature = "oracle")), allow(dead_code))]
pub(crate) fn redact_driver_message(text: &str, secret: Option<&str>) -> String {
    let Some(secret) = secret.filter(|secret| !secret.is_empty()) else {
        return text.to_string();
    };
    if !text.contains(secret) {
        return text.to_string();
    }
    match first_driver_code(text) {
        Some(code) => {
            format!("{code} (driver message hidden because it contained the password)")
        }
        None => "driver error (message hidden because it contained the password)".to_string(),
    }
}

#[cfg_attr(not(any(test, feature = "oracle")), allow(dead_code))]
fn first_driver_code(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    for index in 0..bytes.len() {
        if let Some(code) =
            code_at(text, index, "ORA-", 5).or_else(|| code_at(text, index, "DPI-", 4))
        {
            return Some(code);
        }
    }
    None
}

#[cfg_attr(not(any(test, feature = "oracle")), allow(dead_code))]
fn code_at<'a>(text: &'a str, index: usize, prefix: &str, digits: usize) -> Option<&'a str> {
    let rest = text.get(index..)?;
    if !rest.starts_with(prefix) {
        return None;
    }
    let end = index + prefix.len() + digits;
    let code = text.get(index..end)?;
    if !code.as_bytes()[prefix.len()..]
        .iter()
        .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let bounded = text
        .as_bytes()
        .get(end)
        .is_none_or(|byte| !byte.is_ascii_digit());
    if bounded {
        Some(code)
    } else {
        None
    }
}

#[cfg(test)]
mod redact_tests {
    use super::redact_driver_message;

    const ORA_01017: &str = "ORA-01017: invalid username/password; logon denied";

    #[test]
    fn password_that_is_an_error_prefix_hides_the_whole_message() {
        let hidden = redact_driver_message(ORA_01017, Some("ORA"));
        assert_eq!(
            hidden,
            "ORA-01017 (driver message hidden because it contained the password)"
        );
        assert!(!hidden.contains("***"));
    }

    #[test]
    fn password_absent_from_the_message_keeps_the_text() {
        assert_eq!(redact_driver_message(ORA_01017, Some("zz")), ORA_01017);
    }

    #[test]
    fn empty_or_missing_password_keeps_the_text() {
        assert_eq!(redact_driver_message(ORA_01017, Some("")), ORA_01017);
        assert_eq!(redact_driver_message(ORA_01017, None), ORA_01017);
    }

    #[test]
    fn message_without_a_driver_code_uses_the_fixed_sentence() {
        assert_eq!(
            redact_driver_message("logon denied for secret", Some("secret")),
            "driver error (message hidden because it contained the password)"
        );
    }
}
