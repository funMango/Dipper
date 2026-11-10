//! Local DuckDB store. This module never talks to a source database.

mod load;
mod meta;

use std::path::{Path, PathBuf};

use crate::config::Performance;
use crate::error::DipperError;

pub use load::{type_of, widen, ColumnType, TableLoad};
pub use meta::{RunRecord, RUNS_TABLE};

const LIST_STAGING: &str = "SELECT table_name FROM duckdb_tables() WHERE schema_name = 'main' AND table_name LIKE '%\\_\\_staging' ESCAPE '\\'";

/// DuckDB file owned by Dipper. Source drivers are not used here.
pub struct Store {
    conn: duckdb::Connection,
    path: PathBuf,
}

impl Store {
    /// Create parent directories, apply the memory and temp settings, then drop leftover staging tables.
    pub fn open(path: &Path, perf: &Performance) -> Result<Self, DipperError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(store_err)?;
            }
        }
        std::fs::create_dir_all(&perf.temp_directory).map_err(store_err)?;
        let limit = duckdb_memory_limit(&perf.memory_limit)?;
        let temp = path_text(&perf.temp_directory)?;
        let config = duckdb::Config::default()
            .max_memory(&limit)
            .map_err(store_err)?
            .with("temp_directory", temp)
            .map_err(store_err)?
            .enable_autoload_extension(false)
            .map_err(store_err)?;
        let conn = duckdb::Connection::open_with_flags(path, config).map_err(store_err)?;
        meta::ensure_runs_table(&conn)?;
        cleanup_staging(&conn)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn conn(&self) -> &duckdb::Connection {
        &self.conn
    }

    /// Whether `name` is a user table in `main`. Run history and staging names are not targets.
    pub fn table_exists(&self, name: &str) -> Result<bool, DipperError> {
        let folded = name.to_ascii_lowercase();
        if folded == "_dipper_runs" || folded.ends_with("__staging") {
            return Ok(false);
        }
        let count: i64 = self
            .conn()
            .query_row(
                "SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main' AND lower(table_name) = ?",
                [folded],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        Ok(count > 0)
    }

    /// Start a load into `<table>__staging`, dropping a previous leftover of that name first.
    pub fn begin_load(&self, table: &str) -> Result<TableLoad<'_>, DipperError> {
        let staging = format!("{table}__staging");
        let drop_sql = format!("DROP TABLE IF EXISTS {}", quote(&staging));
        self.conn().execute_batch(&drop_sql).map_err(store_err)?;
        Ok(TableLoad::new(self, table.to_string(), staging))
    }
}

/// Turn a config memory limit into the KiB string DuckDB stores.
///
/// `2GB` is exactly `1953125KiB`. Values below one KiB become `1KiB`.
pub fn duckdb_memory_limit(memory_limit: &str) -> Result<String, DipperError> {
    let bytes = crate::config::parse_memory_limit(memory_limit).map_err(store_err)?;
    let kib = (bytes / 1024).max(1);
    Ok(format!("{kib}KiB"))
}

/// Quote an identifier, doubling embedded quotes.
pub(crate) fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

pub(crate) fn store_err(err: impl std::fmt::Display) -> DipperError {
    DipperError::Store {
        message: err.to_string(),
    }
}

fn path_text(path: &Path) -> Result<&str, DipperError> {
    path.to_str()
        .ok_or_else(|| store_err(format!("path is not valid Unicode: {}", path.display())))
}

fn cleanup_staging(conn: &duckdb::Connection) -> Result<(), DipperError> {
    let mut stmt = conn.prepare(LIST_STAGING).map_err(store_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(store_err)?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row.map_err(store_err)?);
    }
    drop(stmt);
    for name in names {
        let sql = format!("DROP TABLE IF EXISTS {}", quote(&name));
        conn.execute_batch(&sql).map_err(store_err)?;
    }
    Ok(())
}

#[cfg(test)]
impl Store {
    pub(crate) fn testing_table_names(&self) -> Result<Vec<String>, DipperError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT table_name FROM duckdb_tables() WHERE schema_name = 'main' ORDER BY table_name",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(store_err)?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row.map_err(store_err)?);
        }
        Ok(names)
    }

    pub(crate) fn testing_count(&self, table: &str) -> Result<u64, DipperError> {
        let sql = format!("SELECT count(*) FROM {}", quote(table));
        let count: i64 = self
            .conn
            .query_row(&sql, [], |row| row.get(0))
            .map_err(store_err)?;
        u64::try_from(count).map_err(|_| store_err("negative row count"))
    }

    pub(crate) fn testing_runs(&self) -> Result<Vec<TestingRun>, DipperError> {
        let mut stmt = self
            .conn
            .prepare("SELECT ok, error FROM \"_dipper_runs\" ORDER BY finished_at")
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TestingRun {
                    ok: row.get(0)?,
                    error: row.get(1)?,
                })
            })
            .map_err(store_err)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row.map_err(store_err)?);
        }
        Ok(runs)
    }
}

#[cfg(test)]
pub(crate) struct TestingRun {
    pub ok: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn performance(dir: &Path, memory: &str) -> Performance {
        Performance {
            batch_rows: 1000,
            memory_limit: memory.to_string(),
            temp_directory: dir.join("tmp"),
            progress_every_rows: 1000,
        }
    }

    fn setting(store: &Store, name: &str) -> String {
        let sql = format!("SELECT current_setting('{name}')");
        store.conn.query_row(&sql, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn memory_limit_string_is_kib() {
        assert_eq!(duckdb_memory_limit("2GB").unwrap(), "1953125KiB");
    }

    #[test]
    fn open_applies_memory_and_temp_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("nested").join("store.duckdb");
        let perf = performance(dir.path(), "512MiB");
        let store = Store::open(&store_path, &perf).unwrap();
        assert!(store_path.is_file());
        let memory = setting(&store, "memory_limit");
        let bytes = crate::config::parse_memory_limit(&memory)
            .unwrap_or_else(|err| panic!("setting {memory:?}: {err}"));
        assert_eq!(bytes, 512 * 1024 * 1024, "setting {memory}");
        let temp = setting(&store, "temp_directory");
        assert_eq!(
            PathBuf::from(&temp).canonicalize().unwrap(),
            perf.temp_directory.canonicalize().unwrap(),
            "temp setting {temp}"
        );
        drop(store);

        let one_gb = performance(dir.path(), "1GB");
        let store = Store::open(&store_path, &one_gb).unwrap();
        let memory = setting(&store, "memory_limit");
        let bytes = crate::config::parse_memory_limit(&memory)
            .unwrap_or_else(|err| panic!("setting {memory:?}: {err}"));
        let expected = 1_000_000_000u64;
        let diff = bytes.abs_diff(expected);
        assert!(diff * 100 <= expected, "setting {memory} parsed {bytes}");
    }

    #[test]
    fn open_drops_leftover_staging_tables() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store.duckdb");
        let perf = performance(dir.path(), "64MiB");
        let store = Store::open(&store_path, &perf).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TABLE kept (id BIGINT); CREATE TABLE x__staging (id BIGINT); CREATE TABLE plain_staging (id BIGINT)",
            )
            .unwrap();
        drop(store);

        let store = Store::open(&store_path, &perf).unwrap();
        let names = store.testing_table_names().unwrap();
        assert!(names.iter().any(|name| name == "kept"));
        assert!(names.iter().any(|name| name == "plain_staging"));
        assert!(!names.iter().any(|name| name.ends_with("__staging")));
    }
}
