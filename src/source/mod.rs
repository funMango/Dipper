mod sqlite;

#[cfg(feature = "oracle")]
mod oracle;

use crate::config::{Config, Filter, SourceConfig};
use crate::error::DipperError;

pub use sqlite::SqliteSource;

/// What a later PR will ask a source to read. PR2 does not execute it.
#[derive(Debug)]
pub struct SelectSpec {
    pub table: String,
    pub columns: Vec<String>,
    pub filters: Vec<Filter>,
    pub limit: Option<u64>,
    pub batch_rows: usize,
}

/// A scalar value read from a source. Dates and timestamps arrive in PR3.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

pub type ChunkIter<'a> = Box<dyn Iterator<Item = Result<Chunk, DipperError>> + 'a>;

pub trait Source {
    fn name(&self) -> &str;
    fn kind(&self) -> &'static str;
    fn test_connection(&self) -> Result<(), DipperError>;
    fn select<'a>(&'a self, spec: &SelectSpec) -> Result<ChunkIter<'a>, DipperError>;
}

#[derive(Debug)]
pub struct CheckResult {
    pub name: String,
    pub kind: &'static str,
    pub outcome: Result<(), String>,
}

pub fn open(name: &str, cfg: &SourceConfig) -> Result<Box<dyn Source>, DipperError> {
    match cfg {
        SourceConfig::Sqlite { path } => Ok(Box::new(SqliteSource::open(name, path)?)),
        SourceConfig::Oracle {
            connect,
            user,
            password,
        } => open_oracle(name, connect, user, password),
    }
}

#[cfg(feature = "oracle")]
fn open_oracle(
    name: &str,
    connect: &str,
    user: &str,
    password: &crate::config::Secret,
) -> Result<Box<dyn Source>, DipperError> {
    Ok(Box::new(oracle::OracleSource::connect(
        name, connect, user, password,
    )?))
}

#[cfg(not(feature = "oracle"))]
fn open_oracle(
    name: &str,
    connect: &str,
    user: &str,
    password: &crate::config::Secret,
) -> Result<Box<dyn Source>, DipperError> {
    let _ = (connect, user, password);
    Err(DipperError::Source {
        source_name: name.to_string(),
        message: "oracle support is not built in (build with --features oracle)".to_string(),
    })
}

/// Open every source and test it. One failure does not stop the rest.
/// Results follow source name order.
pub fn check_all(config: &Config) -> Vec<CheckResult> {
    config
        .sources
        .iter()
        .map(|(name, cfg)| CheckResult {
            name: name.clone(),
            kind: kind_of(cfg),
            outcome: match open(name, cfg) {
                Ok(source) => source.test_connection().map_err(failure_reason),
                Err(err) => Err(failure_reason(err)),
            },
        })
        .collect()
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
