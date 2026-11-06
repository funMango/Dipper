use crate::error::DipperError;
use crate::source::{Paging, SelectSpec, Value};
use crate::sql::guard::{GuardedSelect, SqlDialect};

/// How serious a preflight or session finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Note,
    Warn,
    Refuse,
}

/// One check result attached to a source session or a dip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub code: &'static str,
    pub dip: Option<String>,
    pub message: String,
}

/// Where a paging key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Declared,
    PrimaryKey,
    UniqueNotNull,
    Rowid,
}

/// Column used to page a dip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKey {
    pub column: String,
    pub source: KeySource,
}

/// Result of checking one dip before it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    pub findings: Vec<Finding>,
    pub key: Option<ResolvedKey>,
    pub target: String,
}

/// Refuse always stops. A warning stops only in strict mode.
pub fn blocks(report: &PreflightReport, strict: bool) -> bool {
    report
        .findings
        .iter()
        .any(|finding| finding.level == Level::Refuse || (strict && finding.level == Level::Warn))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum DictQuery {
    UserUsers,
    SessionPrivs,
    SessionRoles,
    TabPrivsRecd,
    RoleTabPrivs,
    AllObjects,
    AllSynonyms,
    AllDependencies,
    AllTabCols,
    AllConstraints,
    AllConsColumns,
    AllIndexes,
    AllIndColumns,
    SqliteMaster,
}

impl DictQuery {
    pub(crate) fn sql(self) -> &'static str {
        match self {
            DictQuery::UserUsers => r#"SELECT "USERNAME" FROM "USER_USERS""#,
            DictQuery::SessionPrivs => r#"SELECT "PRIVILEGE" FROM "SESSION_PRIVS""#,
            DictQuery::SessionRoles => r#"SELECT "ROLE" FROM "SESSION_ROLES""#,
            DictQuery::TabPrivsRecd => {
                r#"SELECT "PRIVILEGE" FROM "USER_TAB_PRIVS_RECD" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#
            }
            DictQuery::RoleTabPrivs => {
                r#"SELECT "ROLE", "PRIVILEGE" FROM "ROLE_TAB_PRIVS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#
            }
            DictQuery::AllObjects => {
                r#"SELECT "OBJECT_TYPE" FROM "ALL_OBJECTS" WHERE "OWNER" = :1 AND "OBJECT_NAME" = :2"#
            }
            DictQuery::AllSynonyms => {
                r#"SELECT "TABLE_OWNER", "TABLE_NAME", "DB_LINK" FROM "ALL_SYNONYMS" WHERE "OWNER" = :1 AND "SYNONYM_NAME" = :2"#
            }
            DictQuery::AllDependencies => {
                r#"SELECT "REFERENCED_OWNER", "REFERENCED_NAME", "REFERENCED_TYPE", "REFERENCED_LINK_NAME" FROM "ALL_DEPENDENCIES" WHERE "OWNER" = :1 AND "NAME" = :2 AND "TYPE" = :3"#
            }
            DictQuery::AllTabCols => {
                r#"SELECT "COLUMN_NAME", "VIRTUAL_COLUMN", "NULLABLE" FROM "ALL_TAB_COLS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#
            }
            DictQuery::AllConstraints => {
                r#"SELECT "CONSTRAINT_NAME", "CONSTRAINT_TYPE" FROM "ALL_CONSTRAINTS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2 AND "STATUS" = :3"#
            }
            DictQuery::AllConsColumns => {
                r#"SELECT "CONSTRAINT_NAME", "COLUMN_NAME" FROM "ALL_CONS_COLUMNS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#
            }
            DictQuery::AllIndexes => {
                r#"SELECT "OWNER", "INDEX_NAME" FROM "ALL_INDEXES" WHERE "TABLE_OWNER" = :1 AND "TABLE_NAME" = :2 AND "UNIQUENESS" = :3"#
            }
            DictQuery::AllIndColumns => {
                r#"SELECT "INDEX_OWNER", "INDEX_NAME", "COLUMN_NAME" FROM "ALL_IND_COLUMNS" WHERE "TABLE_OWNER" = :1 AND "TABLE_NAME" = :2"#
            }
            DictQuery::SqliteMaster => {
                r#"SELECT "type", "sql" FROM "sqlite_master" WHERE "name" = ?"#
            }
        }
    }

    pub(crate) fn dialect(self) -> SqlDialect {
        match self {
            DictQuery::SqliteMaster => SqlDialect::Sqlite,
            _ => SqlDialect::Oracle,
        }
    }
}

pub(crate) trait Dictionary {
    fn rows(&self, query: DictQuery, binds: &[Value]) -> Result<Vec<Vec<Value>>, DipperError>;
}

pub(crate) fn parse_dict(query: DictQuery) -> Result<GuardedSelect, DipperError> {
    Ok(GuardedSelect::parse(query.sql(), query.dialect())?)
}

/// SQLite object check. Views warn. `WITHOUT ROWID` and a missing object refuse.
pub(crate) fn run_sqlite(dict: &dyn Dictionary, spec: &SelectSpec, dip: &str) -> PreflightReport {
    let mut findings = Vec::new();
    let (schema, name) = match split_sqlite_name(&spec.table) {
        Ok(parts) => parts,
        Err(message) => {
            findings.push(finding(Level::Refuse, "object-not-found", dip, message));
            return PreflightReport {
                findings,
                key: None,
                target: spec.table.clone(),
            };
        }
    };
    let _ = schema;
    let rows = match dict.rows(DictQuery::SqliteMaster, &[Value::Text(name.to_string())]) {
        Ok(rows) => rows,
        Err(err) => {
            findings.push(finding(
                Level::Warn,
                "unverifiable",
                dip,
                format!("could not verify object safety: {err}"),
            ));
            return PreflightReport {
                findings,
                key: None,
                target: name.to_string(),
            };
        }
    };
    let Some(row) = rows.first() else {
        findings.push(finding(
            Level::Refuse,
            "object-not-found",
            dip,
            format!("object not found: {name}"),
        ));
        return PreflightReport {
            findings,
            key: None,
            target: name.to_string(),
        };
    };
    let kind = text_at(row, 0).unwrap_or_default();
    let ddl = text_at(row, 1).unwrap_or_default();
    let is_view = kind.eq_ignore_ascii_case("view");
    if is_view {
        findings.push(finding(
            Level::Warn,
            "view",
            dip,
            format!("target is a view ({name})"),
        ));
    }
    let without_rowid = ddl.to_ascii_lowercase().contains("without rowid");
    let key = if let Some(column) = spec.key.clone() {
        findings.push(finding(
            Level::Warn,
            "key-unverified",
            dip,
            format!("key {column} uniqueness was not verified"),
        ));
        Some(ResolvedKey {
            column,
            source: KeySource::Declared,
        })
    } else if is_view || without_rowid {
        if spec.paging == Paging::Keyset {
            findings.push(finding(
                Level::Refuse,
                "no-key",
                dip,
                "no key; set key or paging = \"snapshot\"".to_string(),
            ));
        }
        None
    } else {
        Some(ResolvedKey {
            column: "rowid".to_string(),
            source: KeySource::Rowid,
        })
    };
    PreflightReport {
        findings,
        key,
        target: name.to_string(),
    }
}

fn split_sqlite_name(table: &str) -> Result<(Option<&str>, &str), String> {
    let mut parts = table.split('.');
    let first = parts.next().unwrap_or("");
    let second = parts.next();
    if parts.next().is_some() {
        return Err(format!(
            "schema prefix in {table} is not allowed; only main is allowed"
        ));
    }
    match second {
        None => Ok((None, first)),
        Some(name) if first == "main" => Ok((Some(first), name)),
        Some(_) => Err(format!(
            "schema prefix {first} is not allowed; only main is allowed"
        )),
    }
}

fn text_at(row: &[Value], index: usize) -> Option<String> {
    match row.get(index) {
        Some(Value::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

fn finding(level: Level, code: &'static str, dip: &str, message: String) -> Finding {
    Finding {
        level,
        code,
        dip: Some(dip.to_string()),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_queries_parse() {
        for query in [
            DictQuery::UserUsers,
            DictQuery::SessionPrivs,
            DictQuery::SessionRoles,
            DictQuery::TabPrivsRecd,
            DictQuery::RoleTabPrivs,
            DictQuery::AllObjects,
            DictQuery::AllSynonyms,
            DictQuery::AllDependencies,
            DictQuery::AllTabCols,
            DictQuery::AllConstraints,
            DictQuery::AllConsColumns,
            DictQuery::AllIndexes,
            DictQuery::AllIndColumns,
            DictQuery::SqliteMaster,
        ] {
            parse_dict(query).unwrap_or_else(|err| panic!("{query:?}: {err}"));
            assert!(!query.sql().contains('\''), "{}", query.sql());
        }
    }
}
