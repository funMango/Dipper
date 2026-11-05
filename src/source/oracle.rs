use crate::config::Secret;
use crate::error::DipperError;
use crate::source::{ChunkIter, SelectSpec, Source};

pub(crate) struct OracleSource {
    name: String,
    conn: oracle::Connection,
}

impl OracleSource {
    pub(crate) fn connect(
        name: &str,
        connect: &str,
        user: &str,
        password: &Secret,
    ) -> Result<Self, DipperError> {
        match oracle::Connection::connect(user, password.expose(), connect) {
            Ok(conn) => Ok(Self {
                name: name.to_string(),
                conn,
            }),
            Err(err) => Err(oracle_error(name, &err)),
        }
    }
}

impl Source for OracleSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &'static str {
        "oracle"
    }

    fn test_connection(&self) -> Result<(), DipperError> {
        self.conn
            .query_row_as::<i32>("SELECT 1 FROM DUAL", &[])
            .map(|_| ())
            .map_err(|err| oracle_error(&self.name, &err))
    }

    fn select<'a>(&'a self, spec: &SelectSpec) -> Result<ChunkIter<'a>, DipperError> {
        let _ = spec;
        Err(DipperError::NotImplemented("select"))
    }
}

fn oracle_error(name: &str, err: &oracle::Error) -> DipperError {
    let text = err.to_string();
    let message = if text.contains("DPI-1047") {
        format!("{text}; Oracle Instant Client not found; see docs/ORACLE_SETUP.md")
    } else {
        text
    };
    DipperError::Source {
        source_name: name.to_string(),
        message,
    }
}
