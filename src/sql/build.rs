use crate::error::DipperError;
use crate::source::{SelectSpec, Value};
use crate::sql::dialect::{placeholder, quote};
use crate::sql::filter::where_clause;
use crate::sql::guard::{GuardedSelect, SqlDialect};

/// A checked plan for reading one dip. Limit and key binds are appended at execution time.
#[derive(Debug, Clone)]
pub struct ReadPlan {
    pub first: GuardedSelect,
    pub next: Option<GuardedSelect>,
    pub binds: Vec<Value>,
    pub key_pos: Option<usize>,
    pub strip_key: bool,
    pub batch_rows: usize,
    pub limit: Option<u64>,
}

/// Keyset pages. `next` repeats the same shape with `key > :last` in front of the row limit.
pub fn keyset(dialect: SqlDialect, spec: &SelectSpec, key: &str) -> Result<ReadPlan, DipperError> {
    let (columns, key_pos, strip_key) = key_columns(&spec.columns, key);
    let (where_sql, binds) = filter_sql(dialect, spec)?;
    let limit_at = binds.len() + 1;
    let key_at = binds.len() + 1;
    let next_limit_at = binds.len() + 2;
    let list = projection(dialect, &columns);
    let from = quote(dialect, &spec.table);
    let order = quote(dialect, key);
    let first_where = match &where_sql {
        Some(filter) => format!(" WHERE {filter}"),
        None => String::new(),
    };
    let next_where = match &where_sql {
        Some(filter) => format!(
            " WHERE {filter} AND {order} > {}",
            placeholder(dialect, key_at)
        ),
        None => format!(" WHERE {order} > {}", placeholder(dialect, key_at)),
    };
    let first = format!(
        "SELECT {list} FROM {from}{first_where} ORDER BY {order}{}",
        row_limit(dialect, limit_at)
    );
    let next = format!(
        "SELECT {list} FROM {from}{next_where} ORDER BY {order}{}",
        row_limit(dialect, next_limit_at)
    );
    Ok(ReadPlan {
        first: parse(dialect, &first)?,
        next: Some(parse(dialect, &next)?),
        binds,
        key_pos: Some(key_pos),
        strip_key,
        batch_rows: spec.batch_rows,
        limit: spec.limit,
    })
}

/// One statement for the whole read. A limit becomes `FETCH FIRST` or `LIMIT`.
pub fn snapshot(dialect: SqlDialect, spec: &SelectSpec) -> Result<ReadPlan, DipperError> {
    let (where_sql, binds) = filter_sql(dialect, spec)?;
    let list = projection(dialect, &spec.columns);
    let from = quote(dialect, &spec.table);
    let where_sql = match where_sql {
        Some(filter) => format!(" WHERE {filter}"),
        None => String::new(),
    };
    let limit_sql = if spec.limit.is_some() {
        row_limit(dialect, binds.len() + 1)
    } else {
        String::new()
    };
    let sql = format!("SELECT {list} FROM {from}{where_sql}{limit_sql}");
    Ok(ReadPlan {
        first: parse(dialect, &sql)?,
        next: None,
        binds,
        key_pos: None,
        strip_key: false,
        batch_rows: spec.batch_rows,
        limit: spec.limit,
    })
}

/// `SELECT COUNT(*)` for the filtered rows. The limit is not applied.
pub fn count(
    dialect: SqlDialect,
    spec: &SelectSpec,
) -> Result<(GuardedSelect, Vec<Value>), DipperError> {
    let (where_sql, binds) = filter_sql(dialect, spec)?;
    let from = quote(dialect, &spec.table);
    let where_sql = match where_sql {
        Some(filter) => format!(" WHERE {filter}"),
        None => String::new(),
    };
    let sql = format!("SELECT COUNT(*) FROM {from}{where_sql}");
    Ok((parse(dialect, &sql)?, binds))
}

fn key_columns(columns: &[String], key: &str) -> (Vec<String>, usize, bool) {
    if let Some(pos) = columns.iter().position(|column| column == key) {
        (columns.to_vec(), pos, false)
    } else {
        let mut with_key = Vec::with_capacity(columns.len() + 1);
        with_key.push(key.to_string());
        with_key.extend(columns.iter().cloned());
        (with_key, 0, true)
    }
}

fn filter_sql(
    dialect: SqlDialect,
    spec: &SelectSpec,
) -> Result<(Option<String>, Vec<Value>), DipperError> {
    if spec.filters.is_empty() {
        Ok((None, Vec::new()))
    } else {
        let (sql, binds) = where_clause(dialect, &spec.filters, 1)?;
        Ok((Some(sql), binds))
    }
}

fn projection(dialect: SqlDialect, columns: &[String]) -> String {
    columns
        .iter()
        .map(|column| quote(dialect, column))
        .collect::<Vec<_>>()
        .join(", ")
}

fn row_limit(dialect: SqlDialect, index: usize) -> String {
    match dialect {
        SqlDialect::Oracle => format!(" FETCH FIRST {} ROWS ONLY", placeholder(dialect, index)),
        SqlDialect::Sqlite => format!(" LIMIT {}", placeholder(dialect, index)),
    }
}

fn parse(dialect: SqlDialect, sql: &str) -> Result<GuardedSelect, DipperError> {
    Ok(GuardedSelect::parse(sql, dialect)?)
}
