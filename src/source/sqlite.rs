use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::error::DipperError;
use crate::source::{ChunkIter, SelectSpec, Source};

pub struct SqliteSource {
    name: String,
    conn: Connection,
}

impl SqliteSource {
    pub fn open(name: &str, path: &Path) -> Result<Self, DipperError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(|err| DipperError::Source {
            source_name: name.to_string(),
            message: format!("cannot open sqlite file {}: {err}", path.display()),
        })?;
        Ok(Self {
            name: name.to_string(),
            conn,
        })
    }

    fn source_error(&self, err: rusqlite::Error) -> DipperError {
        DipperError::Source {
            source_name: self.name.clone(),
            message: err.to_string(),
        }
    }
}

impl Source for SqliteSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn test_connection(&self) -> Result<(), DipperError> {
        let mut stmt = self
            .conn
            .prepare("SELECT 1 FROM sqlite_master LIMIT 1")
            .map_err(|err| self.source_error(err))?;
        let mut rows = stmt.query([]).map_err(|err| self.source_error(err))?;
        while rows.next().map_err(|err| self.source_error(err))?.is_some() {}
        Ok(())
    }

    fn select<'a>(&'a self, spec: &SelectSpec) -> Result<ChunkIter<'a>, DipperError> {
        let _ = spec;
        Err(DipperError::NotImplemented("select"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ErrorCode;

    #[test]
    fn writes_fail_with_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE t (id INTEGER)", []).unwrap();
        conn.execute("INSERT INTO t (id) VALUES (1)", []).unwrap();
        drop(conn);

        let src = SqliteSource::open("t", &path).unwrap();
        let create_err = src
            .conn
            .execute("CREATE TABLE u (id INTEGER)", [])
            .unwrap_err();
        assert_eq!(create_err.sqlite_error_code(), Some(ErrorCode::ReadOnly));
        let insert_err = src
            .conn
            .execute("INSERT INTO t (id) VALUES (2)", [])
            .unwrap_err();
        assert_eq!(insert_err.sqlite_error_code(), Some(ErrorCode::ReadOnly));
    }

    #[test]
    fn missing_file_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.db");
        let Err(err) = SqliteSource::open("missing", &path) else {
            panic!("missing sqlite file was opened");
        };
        let message = err.to_string();
        assert!(message.contains("cannot open sqlite file"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(!path.exists());
    }
}
