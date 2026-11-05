use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Password held so `Debug` cannot print it. There is no `Display` impl.
pub struct Secret(String);

impl Secret {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub(crate) store: Option<String>,
    pub(crate) performance: Option<RawPerformance>,
    pub(crate) sources: BTreeMap<String, RawSource>,
    #[serde(default)]
    pub(crate) dips: Vec<RawDip>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPerformance {
    pub(crate) batch_rows: Option<u64>,
    pub(crate) memory_limit: Option<String>,
    pub(crate) temp_directory: Option<String>,
    pub(crate) progress_every_rows: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum RawSource {
    Sqlite {
        path: String,
    },
    Oracle {
        connect: String,
        user: String,
        password: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawDip {
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) table: String,
    pub(crate) columns: Vec<String>,
    #[serde(default)]
    pub(crate) filters: Vec<RawFilter>,
    pub(crate) limit: Option<u64>,
    pub(crate) count_first: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawFilter {
    pub(crate) column: String,
    pub(crate) op: FilterOp,
    pub(crate) value: Option<toml::Value>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    Ne,
    In,
    Gt,
    Gte,
    Lt,
    Lte,
    Between,
    Like,
    IsNull,
    NotNull,
}

#[derive(Debug)]
pub struct Config {
    pub file: PathBuf,
    pub store: PathBuf,
    pub performance: Performance,
    pub sources: BTreeMap<String, SourceConfig>,
    pub dips: Vec<Dip>,
}

#[derive(Debug)]
pub struct Performance {
    pub batch_rows: usize,
    pub memory_limit: String,
    pub temp_directory: PathBuf,
    pub progress_every_rows: u64,
}

#[derive(Debug)]
pub enum SourceConfig {
    Sqlite {
        path: PathBuf,
    },
    Oracle {
        connect: String,
        user: String,
        password: Secret,
    },
}

#[derive(Debug)]
pub struct Dip {
    pub name: String,
    pub source: String,
    pub table: String,
    pub columns: Vec<String>,
    pub filters: Vec<Filter>,
    pub limit: Option<u64>,
    pub count_first: bool,
}

#[derive(Debug)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub value: Option<toml::Value>,
}

pub(crate) const DEFAULT_STORE: &str = ".dipper/store.duckdb";
pub(crate) const DEFAULT_BATCH_ROWS: usize = 10_000;
pub(crate) const DEFAULT_MEMORY_LIMIT: &str = "2GB";
pub(crate) const DEFAULT_TEMP_DIRECTORY: &str = ".dipper/tmp";
pub(crate) const DEFAULT_PROGRESS_EVERY_ROWS: u64 = 1_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_hides_value() {
        let secret = Secret::new("PR2-canary-7f1d9c".to_string());
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "Secret(***)");
        assert!(!rendered.contains("PR2-canary-7f1d9c"));
    }
}
