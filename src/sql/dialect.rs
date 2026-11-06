use super::guard::SqlDialect;
use crate::sql::quote_ident;

/// Quote an identifier with `"…"` in both dialects.
pub fn quote(dialect: SqlDialect, ident: &str) -> String {
    match dialect {
        SqlDialect::Oracle | SqlDialect::Sqlite => quote_ident(ident),
    }
}

/// Oracle binds are `:1`, `:2`, … and SQLite binds are `?`.
pub fn placeholder(dialect: SqlDialect, n: usize) -> String {
    match dialect {
        SqlDialect::Oracle => format!(":{n}"),
        SqlDialect::Sqlite => "?".to_string(),
    }
}
