use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::de::{self, Deserializer, Visitor};
use serde::Deserialize;

/// Password held so `Debug` cannot print it. There is no `Display` impl.
pub struct Secret(#[cfg_attr(not(any(test, feature = "oracle")), allow(dead_code))] String);

impl Secret {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    /// The default library build has no Oracle driver, so only tests and the
    /// `oracle` feature read the password.
    #[cfg_attr(not(any(test, feature = "oracle")), allow(dead_code))]
    pub(crate) fn expose(&self) -> &str {
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
    pub(crate) safety: Option<RawSafety>,
    pub(crate) sources: BTreeMap<String, RawSource>,
    #[serde(default)]
    pub(crate) dips: Vec<RawDip>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSafety {
    pub(crate) strict_readonly: Option<bool>,
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
        max_session_minutes: Option<u64>,
    },
    Oracle {
        connect: String,
        user: String,
        password: RawPassword,
        call_timeout_secs: Option<u64>,
        max_session_minutes: Option<u64>,
    },
}

/// Password text from TOML. Non-strings are rejected without copying the value into the error.
pub struct RawPassword(String);

pub(crate) const PASSWORD_MUST_BE_STRING: &str = "password must be a string";

impl RawPassword {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RawPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawPassword(***)")
    }
}

impl<'de> Deserialize<'de> for RawPassword {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(RawPasswordVisitor)
    }
}

struct RawPasswordVisitor;

impl<'de> Visitor<'de> for RawPasswordVisitor {
    type Value = RawPassword;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(RawPassword(value.to_string()))
    }

    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
        Ok(RawPassword(value.to_string()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(RawPassword(value))
    }

    fn visit_bool<E: de::Error>(self, _value: bool) -> Result<Self::Value, E> {
        Err(non_string())
    }

    fn visit_i64<E: de::Error>(self, _value: i64) -> Result<Self::Value, E> {
        Err(non_string())
    }

    fn visit_u64<E: de::Error>(self, _value: u64) -> Result<Self::Value, E> {
        Err(non_string())
    }

    fn visit_i128<E: de::Error>(self, _value: i128) -> Result<Self::Value, E> {
        Err(non_string())
    }

    fn visit_u128<E: de::Error>(self, _value: u128) -> Result<Self::Value, E> {
        Err(non_string())
    }

    fn visit_f64<E: de::Error>(self, _value: f64) -> Result<Self::Value, E> {
        Err(non_string())
    }

    fn visit_bytes<E: de::Error>(self, _value: &[u8]) -> Result<Self::Value, E> {
        Err(non_string())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Err(non_string())
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, _seq: A) -> Result<Self::Value, A::Error> {
        Err(non_string())
    }

    fn visit_map<A: de::MapAccess<'de>>(self, _map: A) -> Result<Self::Value, A::Error> {
        Err(non_string())
    }
}

fn non_string<E: de::Error>() -> E {
    E::custom(PASSWORD_MUST_BE_STRING)
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
    pub(crate) key: Option<String>,
    pub(crate) paging: Option<Paging>,
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
    pub strict_readonly: bool,
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
        max_session_minutes: u64,
    },
    Oracle {
        connect: String,
        user: String,
        password: Secret,
        call_timeout_secs: u64,
        max_session_minutes: u64,
    },
}

#[derive(Debug, Clone)]
pub struct Dip {
    pub name: String,
    pub source: String,
    pub table: String,
    pub columns: Vec<String>,
    pub filters: Vec<Filter>,
    pub limit: Option<u64>,
    pub count_first: bool,
    pub key: Option<String>,
    pub paging: Paging,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Paging {
    Keyset,
    Snapshot,
}

#[derive(Debug, Clone)]
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
pub(crate) const DEFAULT_CALL_TIMEOUT_SECS: u64 = 300;
pub(crate) const DEFAULT_MAX_SESSION_MINUTES: u64 = 480;

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
