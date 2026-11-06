#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{HashSet, VecDeque};

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

const READ_PRIVILEGES: &[&str] = &[
    "CREATE SESSION",
    "SELECT ANY TABLE",
    "READ ANY TABLE",
    "SELECT ANY DICTIONARY",
    "SET CONTAINER",
];

const PLSQL_TYPES: &[&str] = &[
    "FUNCTION",
    "PROCEDURE",
    "PACKAGE",
    "PACKAGE BODY",
    "TYPE",
    "TYPE BODY",
    "JAVA CLASS",
    "JAVA SOURCE",
    "LIBRARY",
    "OPERATOR",
    "INDEXTYPE",
];

/// Oracle dictionary check. Findings follow the read-only policy table.
pub(crate) fn run_oracle(
    dict: &dyn Dictionary,
    server_major: i32,
    spec: &SelectSpec,
    dip: &str,
) -> PreflightReport {
    let mut findings = Vec::new();
    if server_major < 12 {
        let level = if spec.paging == Paging::Snapshot {
            Level::Warn
        } else {
            Level::Refuse
        };
        findings.push(finding(
            level,
            "server-too-old",
            dip,
            format!(
                "server {server_major} is older than 12c; key paging requires Oracle 12c or newer"
            ),
        ));
    }
    if spec.paging == Paging::Snapshot {
        findings.push(finding(
            Level::Warn,
            "snapshot",
            dip,
            "snapshot paging can raise ORA-01555 on large tables".to_string(),
        ));
    }
    let username = match text_column(dict, DictQuery::UserUsers, &[], 0, &mut findings, dip) {
        Some(name) => name,
        None => {
            return PreflightReport {
                findings,
                key: None,
                target: spec.table.clone(),
            };
        }
    };
    let (owner, name) = match qualify(&spec.table, &username) {
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
    let Some(target) = resolve(dict, &owner, &name, 0, &mut findings, dip) else {
        return PreflightReport {
            findings,
            key: None,
            target: format!("{owner}.{name}"),
        };
    };
    if target.kind == ObjectKind::View {
        let before = findings.len();
        walk_view(dict, &target, &mut findings, dip);
        let refused = findings[before..]
            .iter()
            .any(|item| item.level == Level::Refuse);
        if !refused {
            findings.push(finding(
                Level::Warn,
                "view",
                dip,
                format!("target is a view ({})", target.name),
            ));
        }
    } else {
        check_columns(dict, &target, spec, &mut findings, dip);
    }
    check_privileges(dict, &username, &target, &mut findings, dip);
    let key = resolve_key(dict, &target, spec, &mut findings, dip);
    PreflightReport {
        findings,
        key,
        target: format!("{}.{}", target.owner, target.name),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ObjectKind {
    Table,
    View,
}

struct ObjectRef {
    owner: String,
    name: String,
    kind: ObjectKind,
}

fn qualify(table: &str, user: &str) -> Result<(String, String), String> {
    let parts: Vec<&str> = table.split('.').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [name] => Ok((user.to_string(), (*name).to_string())),
        [owner, name] => Ok(((*owner).to_string(), (*name).to_string())),
        _ => Err(format!(
            "object not found: {table}; names are case-sensitive; Oracle stores unquoted names in uppercase"
        )),
    }
}

fn resolve(
    dict: &dyn Dictionary,
    owner: &str,
    name: &str,
    depth: usize,
    findings: &mut Vec<Finding>,
    dip: &str,
) -> Option<ObjectRef> {
    if depth > 5 {
        findings.push(finding(
            Level::Refuse,
            "synonym-chain-too-long",
            dip,
            "synonym chain exceeds 5 steps".to_string(),
        ));
        return None;
    }
    let objects = match dict.rows(
        DictQuery::AllObjects,
        &[
            Value::Text(owner.to_string()),
            Value::Text(name.to_string()),
        ],
    ) {
        Ok(rows) => rows,
        Err(err) => {
            unverifiable(findings, dip, &err);
            return None;
        }
    };
    let types = column_texts(&objects, 0);
    if types
        .iter()
        .any(|kind| kind == "TABLE" || kind == "MATERIALIZED VIEW")
    {
        return Some(ObjectRef {
            owner: owner.to_string(),
            name: name.to_string(),
            kind: ObjectKind::Table,
        });
    }
    if types.iter().any(|kind| kind == "VIEW") {
        return Some(ObjectRef {
            owner: owner.to_string(),
            name: name.to_string(),
            kind: ObjectKind::View,
        });
    }
    if !types.is_empty() && types.iter().all(|kind| kind != "SYNONYM") {
        findings.push(finding(
            Level::Refuse,
            "object-not-found",
            dip,
            not_found(owner, name),
        ));
        return None;
    }
    let synonym = match lookup_synonym(dict, owner, name, findings, dip) {
        Some(row) => row,
        None => {
            if types.is_empty() {
                findings.push(finding(
                    Level::Refuse,
                    "object-not-found",
                    dip,
                    not_found(owner, name),
                ));
            }
            return None;
        }
    };
    if synonym.2.is_some() {
        findings.push(finding(
            Level::Refuse,
            "synonym-db-link",
            dip,
            format!("synonym {name} points across a database link"),
        ));
        return None;
    }
    resolve(dict, &synonym.0, &synonym.1, depth + 1, findings, dip)
}

fn lookup_synonym(
    dict: &dyn Dictionary,
    owner: &str,
    name: &str,
    findings: &mut Vec<Finding>,
    dip: &str,
) -> Option<(String, String, Option<String>)> {
    let mut owners = vec![owner.to_string()];
    if owner != "PUBLIC" {
        owners.push("PUBLIC".to_string());
    }
    for candidate in owners {
        match dict.rows(
            DictQuery::AllSynonyms,
            &[Value::Text(candidate), Value::Text(name.to_string())],
        ) {
            Ok(rows) => {
                if let Some(row) = rows.first() {
                    let link = text_at(row, 2).filter(|value| !value.is_empty());
                    return Some((
                        text_at(row, 0).unwrap_or_default(),
                        text_at(row, 1).unwrap_or_default(),
                        link,
                    ));
                }
            }
            Err(err) => {
                unverifiable(findings, dip, &err);
                return None;
            }
        }
    }
    None
}

fn walk_view(dict: &dyn Dictionary, view: &ObjectRef, findings: &mut Vec<Finding>, dip: &str) {
    let mut queue = VecDeque::from([(
        "VIEW".to_string(),
        view.owner.clone(),
        view.name.clone(),
        0usize,
    )]);
    let mut seen = HashSet::from([("VIEW".to_string(), view.owner.clone(), view.name.clone())]);
    while let Some((kind, owner, name, depth)) = queue.pop_front() {
        if seen.len() > 200 || depth > 10 {
            unverifiable(
                findings,
                dip,
                &DipperError::Source {
                    source_name: dip.to_string(),
                    message: "view dependency limit exceeded".to_string(),
                },
            );
            return;
        }
        let rows = match dict.rows(
            DictQuery::AllDependencies,
            &[
                Value::Text(owner.clone()),
                Value::Text(name.clone()),
                Value::Text(kind),
            ],
        ) {
            Ok(rows) => rows,
            Err(err) => {
                unverifiable(findings, dip, &err);
                return;
            }
        };
        for row in rows {
            let ref_owner = text_at(&row, 0).unwrap_or_default();
            let ref_name = text_at(&row, 1).unwrap_or_default();
            let ref_type = text_at(&row, 2).unwrap_or_default();
            let link = text_at(&row, 3).filter(|value| !value.is_empty());
            if link.is_some() {
                findings.push(finding(
                    Level::Refuse,
                    "view-db-link",
                    dip,
                    "view depends on a database link".to_string(),
                ));
                continue;
            }
            if is_plsql(&ref_type) && !(ref_owner == "SYS" && ref_name == "STANDARD") {
                findings.push(finding(
                    Level::Refuse,
                    "view-plsql-dependency",
                    dip,
                    format!("view depends on {ref_type} {ref_owner}.{ref_name}"),
                ));
                continue;
            }
            let follow = if ref_type == "SYNONYM" {
                resolve(dict, &ref_owner, &ref_name, 0, findings, dip)
            } else if ref_type == "VIEW" {
                Some(ObjectRef {
                    owner: ref_owner.clone(),
                    name: ref_name.clone(),
                    kind: ObjectKind::View,
                })
            } else {
                None
            };
            if let Some(next) = follow {
                if next.kind == ObjectKind::View {
                    let key = ("VIEW".to_string(), next.owner.clone(), next.name.clone());
                    if seen.insert(key) {
                        queue.push_back(("VIEW".to_string(), next.owner, next.name, depth + 1));
                    }
                }
            }
        }
    }
}

fn check_columns(
    dict: &dyn Dictionary,
    target: &ObjectRef,
    spec: &SelectSpec,
    findings: &mut Vec<Finding>,
    dip: &str,
) {
    let rows = match dict.rows(
        DictQuery::AllTabCols,
        &[
            Value::Text(target.owner.clone()),
            Value::Text(target.name.clone()),
        ],
    ) {
        Ok(rows) => rows,
        Err(err) => {
            unverifiable(findings, dip, &err);
            return;
        }
    };
    let mut needed = spec.columns.clone();
    if let Some(key) = &spec.key {
        needed.push(key.clone());
    }
    for filter in &spec.filters {
        needed.push(filter.column.clone());
    }
    for column in needed {
        let Some(row) = rows
            .iter()
            .find(|row| text_at(row, 0).as_deref() == Some(column.as_str()))
        else {
            findings.push(finding(
                Level::Refuse,
                "column-not-found",
                dip,
                format!("column {column} not found"),
            ));
            continue;
        };
        if text_at(row, 1).as_deref() == Some("YES") {
            findings.push(finding(
                Level::Refuse,
                "virtual-column",
                dip,
                format!("column {column} is virtual"),
            ));
        }
    }
}

fn check_privileges(
    dict: &dyn Dictionary,
    username: &str,
    target: &ObjectRef,
    findings: &mut Vec<Finding>,
    dip: &str,
) {
    if let Some(privs) = strings(dict, DictQuery::SessionPrivs, &[], 0, findings, dip) {
        let mut extra: Vec<String> = privs
            .into_iter()
            .filter(|privilege| !READ_PRIVILEGES.contains(&privilege.as_str()))
            .collect();
        extra.sort();
        extra.dedup();
        if !extra.is_empty() {
            findings.push(finding(
                Level::Warn,
                "write-system-privilege",
                dip,
                format!("account has write system privileges: {}", extra.join(", ")),
            ));
        }
    }
    if target.owner.eq_ignore_ascii_case(username) {
        findings.push(finding(
            Level::Warn,
            "owns-table",
            dip,
            format!("account owns table {}.{}", target.owner, target.name),
        ));
    }
    let mut object_privs = Vec::new();
    if let Some(privs) = strings(
        dict,
        DictQuery::TabPrivsRecd,
        &[
            Value::Text(target.owner.clone()),
            Value::Text(target.name.clone()),
        ],
        0,
        findings,
        dip,
    ) {
        object_privs.extend(privs);
    }
    if let Some(rows) = rows_ok(
        dict,
        DictQuery::RoleTabPrivs,
        &[
            Value::Text(target.owner.clone()),
            Value::Text(target.name.clone()),
        ],
        findings,
        dip,
    ) {
        object_privs.extend(column_texts(&rows, 1));
    }
    let mut write: Vec<String> = object_privs
        .into_iter()
        .filter(|privilege| privilege != "SELECT" && privilege != "READ")
        .collect();
    write.sort();
    write.dedup();
    if !write.is_empty() {
        findings.push(finding(
            Level::Warn,
            "write-object-privilege",
            dip,
            format!("account has write object privileges: {}", write.join(", ")),
        ));
    }
    if let Some(roles) = strings(dict, DictQuery::SessionRoles, &[], 0, findings, dip) {
        if roles.iter().any(|role| role == "DBA") {
            findings.push(finding(
                Level::Warn,
                "dba-role",
                dip,
                "account has DBA role".to_string(),
            ));
        }
    }
}

fn resolve_key(
    dict: &dyn Dictionary,
    target: &ObjectRef,
    spec: &SelectSpec,
    findings: &mut Vec<Finding>,
    dip: &str,
) -> Option<ResolvedKey> {
    if spec.paging == Paging::Snapshot {
        return spec.key.clone().map(|column| ResolvedKey {
            column,
            source: KeySource::Declared,
        });
    }
    if target.kind == ObjectKind::View {
        return match spec.key.clone() {
            Some(column) => {
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
            }
            None => {
                findings.push(finding(
                    Level::Refuse,
                    "no-key",
                    dip,
                    "no key; set key or paging = \"snapshot\"".to_string(),
                ));
                None
            }
        };
    }
    let columns = rows_ok(
        dict,
        DictQuery::AllTabCols,
        &[
            Value::Text(target.owner.clone()),
            Value::Text(target.name.clone()),
        ],
        findings,
        dip,
    )?;
    let constraints = rows_ok(
        dict,
        DictQuery::AllConstraints,
        &[
            Value::Text(target.owner.clone()),
            Value::Text(target.name.clone()),
            Value::Text("ENABLED".to_string()),
        ],
        findings,
        dip,
    )?;
    let cons_columns = rows_ok(
        dict,
        DictQuery::AllConsColumns,
        &[
            Value::Text(target.owner.clone()),
            Value::Text(target.name.clone()),
        ],
        findings,
        dip,
    )?;
    let indexes = rows_ok(
        dict,
        DictQuery::AllIndexes,
        &[
            Value::Text(target.owner.clone()),
            Value::Text(target.name.clone()),
            Value::Text("UNIQUE".to_string()),
        ],
        findings,
        dip,
    )?;
    let index_columns = rows_ok(
        dict,
        DictQuery::AllIndColumns,
        &[
            Value::Text(target.owner.clone()),
            Value::Text(target.name.clone()),
        ],
        findings,
        dip,
    )?;
    let nullable = |column: &str| {
        columns
            .iter()
            .find(|row| text_at(row, 0).as_deref() == Some(column))
            .and_then(|row| text_at(row, 2))
            .as_deref()
            != Some("N")
    };
    let column_list = |name: &str| -> Vec<String> {
        cons_columns
            .iter()
            .filter(|row| text_at(row, 0).as_deref() == Some(name))
            .filter_map(|row| text_at(row, 1))
            .collect()
    };
    let is_single_pk = |column: &str| {
        constraints.iter().any(|row| {
            text_at(row, 1).as_deref() == Some("P")
                && column_list(&text_at(row, 0).unwrap_or_default()) == [column.to_string()]
        })
    };
    let is_single_unique = |column: &str| {
        let constraint = constraints.iter().any(|row| {
            text_at(row, 1).as_deref() == Some("U")
                && column_list(&text_at(row, 0).unwrap_or_default()) == [column.to_string()]
        });
        let index = indexes.iter().any(|row| {
            let index_name = text_at(row, 1).unwrap_or_default();
            let cols: Vec<String> = index_columns
                .iter()
                .filter(|col| text_at(col, 1).as_deref() == Some(index_name.as_str()))
                .filter_map(|col| text_at(col, 2))
                .collect();
            cols == [column.to_string()]
        });
        constraint || index
    };
    let accept = |column: &str, source: KeySource| ResolvedKey {
        column: column.to_string(),
        source,
    };
    if let Some(column) = spec.key.clone() {
        if is_single_pk(&column) || (!nullable(&column) && is_single_unique(&column)) {
            return Some(accept(
                &column,
                if is_single_pk(&column) {
                    KeySource::PrimaryKey
                } else {
                    KeySource::UniqueNotNull
                },
            ));
        }
        if nullable(&column) {
            findings.push(finding(
                Level::Refuse,
                "key-nullable",
                dip,
                format!("key {column} allows NULL"),
            ));
        } else {
            findings.push(finding(
                Level::Refuse,
                "key-not-unique",
                dip,
                format!("key {column} is not a single-column unique key"),
            ));
        }
        return None;
    }
    if let Some(column) = constraints.iter().find_map(|row| {
        if text_at(row, 1).as_deref() != Some("P") {
            return None;
        }
        let cols = column_list(&text_at(row, 0).unwrap_or_default());
        (cols.len() == 1).then(|| cols[0].clone())
    }) {
        return Some(accept(&column, KeySource::PrimaryKey));
    }
    if let Some(column) = constraints.iter().find_map(|row| {
        if text_at(row, 1).as_deref() != Some("U") {
            return None;
        }
        let cols = column_list(&text_at(row, 0).unwrap_or_default());
        (cols.len() == 1 && !nullable(&cols[0])).then(|| cols[0].clone())
    }) {
        return Some(accept(&column, KeySource::UniqueNotNull));
    }
    if let Some(column) = indexes.iter().find_map(|row| {
        let index_name = text_at(row, 1)?;
        let cols: Vec<String> = index_columns
            .iter()
            .filter(|col| text_at(col, 1).as_deref() == Some(index_name.as_str()))
            .filter_map(|col| text_at(col, 2))
            .collect();
        (cols.len() == 1 && !nullable(&cols[0])).then(|| cols[0].clone())
    }) {
        return Some(accept(&column, KeySource::UniqueNotNull));
    }
    findings.push(finding(
        Level::Refuse,
        "no-key",
        dip,
        "no key; set key or paging = \"snapshot\"".to_string(),
    ));
    None
}

fn is_plsql(kind: &str) -> bool {
    PLSQL_TYPES.contains(&kind)
}

fn not_found(owner: &str, name: &str) -> String {
    format!(
        "object not found: {owner}.{name}; names are case-sensitive; Oracle stores unquoted names in uppercase"
    )
}

fn unverifiable(findings: &mut Vec<Finding>, dip: &str, err: &DipperError) {
    findings.push(finding(
        Level::Warn,
        "unverifiable",
        dip,
        format!("could not verify object safety: {err}"),
    ));
}

fn text_column(
    dict: &dyn Dictionary,
    query: DictQuery,
    binds: &[Value],
    index: usize,
    findings: &mut Vec<Finding>,
    dip: &str,
) -> Option<String> {
    strings(dict, query, binds, index, findings, dip)?
        .into_iter()
        .next()
}

fn strings(
    dict: &dyn Dictionary,
    query: DictQuery,
    binds: &[Value],
    index: usize,
    findings: &mut Vec<Finding>,
    dip: &str,
) -> Option<Vec<String>> {
    rows_ok(dict, query, binds, findings, dip).map(|rows| column_texts(&rows, index))
}

fn rows_ok(
    dict: &dyn Dictionary,
    query: DictQuery,
    binds: &[Value],
    findings: &mut Vec<Finding>,
    dip: &str,
) -> Option<Vec<Vec<Value>>> {
    match dict.rows(query, binds) {
        Ok(rows) => Some(rows),
        Err(err) => {
            unverifiable(findings, dip, &err);
            None
        }
    }
}

fn column_texts(rows: &[Vec<Value>], index: usize) -> Vec<String> {
    rows.iter().filter_map(|row| text_at(row, index)).collect()
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

    type CatRow = (DictQuery, Vec<String>, Vec<Vec<Option<String>>>);

    #[derive(Clone)]
    struct Cat {
        rows: Vec<CatRow>,
        fail: Vec<DictQuery>,
    }

    impl Dictionary for Cat {
        fn rows(&self, query: DictQuery, binds: &[Value]) -> Result<Vec<Vec<Value>>, DipperError> {
            if self.fail.contains(&query) {
                return Err(DipperError::Source {
                    source_name: "dict".to_string(),
                    message: "not permitted".to_string(),
                });
            }
            let bound: Vec<String> = binds
                .iter()
                .map(|value| match value {
                    Value::Text(text) => text.clone(),
                    _ => String::new(),
                })
                .collect();
            Ok(self
                .rows
                .iter()
                .find(|(item, key, _)| *item == query && key == &bound)
                .map(|(_, _, rows)| {
                    rows.iter()
                        .map(|row| {
                            row.iter()
                                .map(|cell| match cell {
                                    Some(text) => Value::Text(text.clone()),
                                    None => Value::Null,
                                })
                                .collect()
                        })
                        .collect()
                })
                .unwrap_or_default())
        }
    }

    fn cells(values: &[&str]) -> Vec<Option<String>> {
        values
            .iter()
            .map(|value| Some((*value).to_string()))
            .collect()
    }

    fn add(cat: &mut Cat, query: DictQuery, binds: &[&str], row: Vec<Option<String>>) {
        let key: Vec<String> = binds.iter().map(|value| (*value).to_string()).collect();
        if let Some((_, _, rows)) = cat
            .rows
            .iter_mut()
            .find(|(item, found, _)| *item == query && found == &key)
        {
            rows.push(row);
        } else {
            cat.rows.push((query, key, vec![row]));
        }
    }

    fn owned_orders() -> Cat {
        let mut cat = Cat {
            rows: Vec::new(),
            fail: Vec::new(),
        };
        add(&mut cat, DictQuery::UserUsers, &[], cells(&["SCOTT"]));
        add(
            &mut cat,
            DictQuery::SessionPrivs,
            &[],
            cells(&["CREATE SESSION"]),
        );
        add(
            &mut cat,
            DictQuery::AllObjects,
            &["SALES", "ORDERS"],
            cells(&["TABLE"]),
        );
        add(
            &mut cat,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["ORDER_ID", "NO", "N"]),
        );
        add(
            &mut cat,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["STATUS", "NO", "Y"]),
        );
        add(
            &mut cat,
            DictQuery::AllConstraints,
            &["SALES", "ORDERS", "ENABLED"],
            cells(&["PK_ORDERS", "P"]),
        );
        add(
            &mut cat,
            DictQuery::AllConsColumns,
            &["SALES", "ORDERS"],
            cells(&["PK_ORDERS", "ORDER_ID"]),
        );
        cat
    }

    fn spec(table: &str, paging: Paging, key: Option<&str>) -> SelectSpec {
        SelectSpec {
            table: table.to_string(),
            columns: vec!["ORDER_ID".to_string(), "STATUS".to_string()],
            filters: Vec::new(),
            limit: None,
            batch_rows: 100,
            key: key.map(str::to_string),
            paging,
        }
    }

    fn run(cat: &Cat, major: i32, spec: &SelectSpec) -> PreflightReport {
        run_oracle(cat, major, spec, "orders")
    }

    fn has(report: &PreflightReport, code: &str, level: Level) -> bool {
        report
            .findings
            .iter()
            .any(|finding| finding.code == code && finding.level == level)
    }

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

    #[test]
    fn primary_key_is_detected_without_warnings() {
        let report = run(
            &owned_orders(),
            19,
            &spec("SALES.ORDERS", Paging::Keyset, None),
        );
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(
            report.key.as_ref().map(|key| key.source),
            Some(KeySource::PrimaryKey)
        );
        assert!(!blocks(&report, false));
        assert!(!blocks(&report, true));
    }

    #[test]
    fn write_privileges_views_and_roles_warn_unless_strict() {
        let mut cat = owned_orders();
        add(
            &mut cat,
            DictQuery::SessionPrivs,
            &[],
            cells(&["CREATE TABLE"]),
        );
        add(
            &mut cat,
            DictQuery::SessionPrivs,
            &[],
            cells(&["INSERT ANY TABLE"]),
        );
        add(&mut cat, DictQuery::SessionRoles, &[], cells(&["DBA"]));
        add(
            &mut cat,
            DictQuery::TabPrivsRecd,
            &["SALES", "ORDERS"],
            cells(&["INSERT"]),
        );
        add(
            &mut cat,
            DictQuery::RoleTabPrivs,
            &["SALES", "ORDERS"],
            cells(&["APP_ROLE", "UPDATE"]),
        );
        let report = run(&cat, 19, &spec("SALES.ORDERS", Paging::Keyset, None));
        assert!(has(&report, "write-system-privilege", Level::Warn));
        assert!(report.findings.iter().any(|finding| {
            finding.code == "write-system-privilege"
                && finding.message.contains("CREATE TABLE")
                && finding.message.contains("INSERT ANY TABLE")
        }));
        assert!(has(&report, "write-object-privilege", Level::Warn));
        assert!(has(&report, "dba-role", Level::Warn));
        assert!(!blocks(&report, false));
        assert!(blocks(&report, true));

        let mut owned = owned_orders();
        add(&mut owned, DictQuery::UserUsers, &[], cells(&["SALES"]));
        owned
            .rows
            .retain(|(query, _, _)| *query != DictQuery::UserUsers);
        add(&mut owned, DictQuery::UserUsers, &[], cells(&["SALES"]));
        let report = run(&owned, 19, &spec("ORDERS", Paging::Keyset, None));
        assert!(has(&report, "owns-table", Level::Warn));
        assert!(!blocks(&report, false));
        assert!(blocks(&report, true));
    }

    #[test]
    fn views_synonyms_and_virtual_columns_follow_the_policy() {
        let mut view = owned_orders();
        view.rows
            .retain(|(query, _, _)| *query != DictQuery::AllObjects);
        add(
            &mut view,
            DictQuery::AllObjects,
            &["SALES", "V_ORDERS"],
            cells(&["VIEW"]),
        );
        add(
            &mut view,
            DictQuery::AllDependencies,
            &["SALES", "V_ORDERS", "VIEW"],
            cells(&["SALES", "ORDERS", "TABLE", ""]),
        );
        let report = run(
            &view,
            19,
            &spec("SALES.V_ORDERS", Paging::Keyset, Some("ORDER_ID")),
        );
        assert!(has(&report, "view", Level::Warn));
        assert!(has(&report, "key-unverified", Level::Warn));
        assert!(!blocks(&report, false));
        assert!(blocks(&report, true));

        let mut linked = view.clone();
        linked
            .rows
            .retain(|(query, _, _)| *query != DictQuery::AllDependencies);
        add(
            &mut linked,
            DictQuery::AllDependencies,
            &["SALES", "V_ORDERS", "VIEW"],
            vec![
                Some("SALES".to_string()),
                Some("ORDERS".to_string()),
                Some("TABLE".to_string()),
                Some("REMOTE".to_string()),
            ],
        );
        let report = run(
            &linked,
            19,
            &spec("SALES.V_ORDERS", Paging::Keyset, Some("ORDER_ID")),
        );
        assert!(has(&report, "view-db-link", Level::Refuse));
        assert!(blocks(&report, false));
        assert!(blocks(&report, true));

        let mut function = view.clone();
        function
            .rows
            .retain(|(query, _, _)| *query != DictQuery::AllDependencies);
        add(
            &mut function,
            DictQuery::AllDependencies,
            &["SALES", "V_ORDERS", "VIEW"],
            cells(&["SALES", "F_AUDIT", "FUNCTION", ""]),
        );
        let report = run(
            &function,
            19,
            &spec("SALES.V_ORDERS", Paging::Keyset, Some("ORDER_ID")),
        );
        assert!(has(&report, "view-plsql-dependency", Level::Refuse));
        assert!(report
            .findings
            .iter()
            .any(|finding| { finding.message.contains("FUNCTION SALES.F_AUDIT") }));

        let mut synonym = owned_orders();
        add(
            &mut synonym,
            DictQuery::AllObjects,
            &["SCOTT", "ORDERS"],
            cells(&["SYNONYM"]),
        );
        add(
            &mut synonym,
            DictQuery::AllSynonyms,
            &["SCOTT", "ORDERS"],
            cells(&["SALES", "ORDERS", ""]),
        );
        let report = run(&synonym, 19, &spec("ORDERS", Paging::Keyset, None));
        assert!(
            !has(&report, "synonym-db-link", Level::Refuse),
            "{:?}",
            report.findings
        );
        assert_eq!(
            report.key.as_ref().map(|key| key.source),
            Some(KeySource::PrimaryKey)
        );

        let mut remote = synonym.clone();
        remote.rows.retain(|(query, binds, _)| {
            !(*query == DictQuery::AllSynonyms
                && binds == &["SCOTT".to_string(), "ORDERS".to_string()])
        });
        add(
            &mut remote,
            DictQuery::AllSynonyms,
            &["SCOTT", "ORDERS"],
            vec![
                Some("SALES".to_string()),
                Some("ORDERS".to_string()),
                Some("DBLINK".to_string()),
            ],
        );
        let report = run(&remote, 19, &spec("ORDERS", Paging::Keyset, None));
        assert!(has(&report, "synonym-db-link", Level::Refuse));
        assert!(blocks(&report, false));

        let mut chain = Cat {
            rows: Vec::new(),
            fail: Vec::new(),
        };
        add(&mut chain, DictQuery::UserUsers, &[], cells(&["SCOTT"]));
        add(
            &mut chain,
            DictQuery::SessionPrivs,
            &[],
            cells(&["CREATE SESSION"]),
        );
        for (from, to) in [
            ("S0", "S1"),
            ("S1", "S2"),
            ("S2", "S3"),
            ("S3", "S4"),
            ("S4", "S5"),
            ("S5", "S6"),
        ] {
            add(
                &mut chain,
                DictQuery::AllSynonyms,
                &["SCOTT", from],
                cells(&["SCOTT", to, ""]),
            );
        }
        let report = run(&chain, 19, &spec("S0", Paging::Keyset, None));
        assert!(
            has(&report, "synonym-chain-too-long", Level::Refuse),
            "{:?}",
            report.findings
        );

        let mut virtual_col = owned_orders();
        virtual_col
            .rows
            .retain(|(query, _, _)| *query != DictQuery::AllTabCols);
        add(
            &mut virtual_col,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["ORDER_ID", "NO", "N"]),
        );
        add(
            &mut virtual_col,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["STATUS", "YES", "Y"]),
        );
        let report = run(
            &virtual_col,
            19,
            &spec("SALES.ORDERS", Paging::Keyset, None),
        );
        assert!(has(&report, "virtual-column", Level::Refuse));
    }

    #[test]
    fn missing_bad_and_old_keys_are_refused() {
        let mut missing = owned_orders();
        missing
            .rows
            .retain(|(query, _, _)| *query != DictQuery::AllObjects);
        let report = run(&missing, 19, &spec("SALES.ORDERS", Paging::Keyset, None));
        assert!(has(&report, "object-not-found", Level::Refuse));
        assert!(report
            .findings
            .iter()
            .any(|finding| { finding.message.contains("names are case-sensitive") }));
        assert!(blocks(&report, false));
        assert!(blocks(&report, true));

        let mut plain = owned_orders();
        plain
            .rows
            .retain(|(query, _, _)| *query != DictQuery::AllConstraints);
        let report = run(&plain, 19, &spec("SALES.ORDERS", Paging::Keyset, None));
        assert!(has(&report, "no-key", Level::Refuse));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.message.contains("snapshot")));

        let mut nullable = owned_orders();
        nullable.rows.retain(|(query, _, _)| {
            *query != DictQuery::AllConstraints && *query != DictQuery::AllTabCols
        });
        add(
            &mut nullable,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["STATUS", "NO", "Y"]),
        );
        add(
            &mut nullable,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["ORDER_ID", "NO", "Y"]),
        );
        add(
            &mut nullable,
            DictQuery::AllConstraints,
            &["SALES", "ORDERS", "ENABLED"],
            cells(&["UQ", "U"]),
        );
        add(
            &mut nullable,
            DictQuery::AllConsColumns,
            &["SALES", "ORDERS"],
            cells(&["UQ", "STATUS"]),
        );
        let report = run(
            &nullable,
            19,
            &spec("SALES.ORDERS", Paging::Keyset, Some("STATUS")),
        );
        assert!(has(&report, "key-nullable", Level::Refuse));

        let mut duplicate = owned_orders();
        add(
            &mut duplicate,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["CODE", "NO", "N"]),
        );
        let report = run(
            &duplicate,
            19,
            &spec("SALES.ORDERS", Paging::Keyset, Some("CODE")),
        );
        assert!(has(&report, "key-not-unique", Level::Refuse));

        let mut unique = owned_orders();
        unique.rows.retain(|(query, _, _)| {
            *query != DictQuery::AllConstraints && *query != DictQuery::AllTabCols
        });
        add(
            &mut unique,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["ORDER_ID", "NO", "N"]),
        );
        add(
            &mut unique,
            DictQuery::AllTabCols,
            &["SALES", "ORDERS"],
            cells(&["STATUS", "NO", "N"]),
        );
        add(
            &mut unique,
            DictQuery::AllConstraints,
            &["SALES", "ORDERS", "ENABLED"],
            cells(&["UQ", "U"]),
        );
        add(
            &mut unique,
            DictQuery::AllConsColumns,
            &["SALES", "ORDERS"],
            cells(&["UQ", "STATUS"]),
        );
        let report = run(&unique, 19, &spec("SALES.ORDERS", Paging::Keyset, None));
        assert_eq!(
            report.key.as_ref().map(|key| key.source),
            Some(KeySource::UniqueNotNull)
        );
        assert!(!blocks(&report, false));

        let old = run(
            &owned_orders(),
            11,
            &spec("SALES.ORDERS", Paging::Keyset, None),
        );
        assert!(has(&old, "server-too-old", Level::Refuse));
        assert!(blocks(&old, false));
        let snap = run(
            &owned_orders(),
            11,
            &spec("SALES.ORDERS", Paging::Snapshot, None),
        );
        assert!(has(&snap, "server-too-old", Level::Warn));
        assert!(has(&snap, "snapshot", Level::Warn));
        assert!(!blocks(&snap, false));
        assert!(blocks(&snap, true));

        let mut failed = owned_orders();
        failed.fail.push(DictQuery::AllObjects);
        let report = run(&failed, 19, &spec("SALES.ORDERS", Paging::Keyset, None));
        assert!(has(&report, "unverifiable", Level::Warn));
        assert!(!blocks(&report, false));
        assert!(blocks(&report, true));
    }
}
