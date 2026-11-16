use dipper::config::{Filter, FilterOp};
use dipper::source::{Paging, SelectSpec};
use dipper::sql::build::{self, ReadPlan};
use dipper::sql::guard::{GuardedOverlapSelect, GuardedSelect, SqlDialect};

fn reject(sql: &str, dialect: SqlDialect) {
    let err = GuardedSelect::parse(sql, dialect).expect_err(sql);
    let message = err.to_string();
    assert!(
        message.starts_with("rejected SQL:"),
        "{sql} ({dialect:?}): {message}"
    );
}

fn reject_both(sql: &str) {
    reject(sql, SqlDialect::Oracle);
    reject(sql, SqlDialect::Sqlite);
}

fn accept(sql: &str, dialect: SqlDialect) {
    let guarded = GuardedSelect::parse(sql, dialect).unwrap_or_else(|err| panic!("{sql}: {err}"));
    assert_eq!(guarded.sql(), sql);
    assert_eq!(guarded.dialect(), dialect);
}

#[test]
fn rejects_statements_that_are_not_a_single_select() {
    let samples = [
        r#"SELECT "A" FROM "T"; DELETE FROM "T""#,
        r#"INSERT INTO "T" ("A") VALUES (:1)"#,
        r#"UPDATE "T" SET "A" = :1"#,
        r#"DELETE FROM "T""#,
        r#"MERGE INTO "T" USING "U" ON ("T"."A" = "U"."A") WHEN MATCHED THEN UPDATE SET "B" = :1"#,
        r#"CREATE TABLE "X" ("A" INT)"#,
        r#"DROP TABLE "T""#,
        r#"ALTER TABLE "T" ADD "B" INT"#,
        r#"TRUNCATE TABLE "T""#,
        r#"GRANT SELECT ON "T" TO "U""#,
        r#"LOCK TABLE "T" IN EXCLUSIVE MODE"#,
        "COMMIT",
        "BEGIN NULL; END;",
        r#"CALL "P"()"#,
        r#"SELECT "A" FROM "T" FOR UPDATE"#,
        r#"SELECT "A" FROM "T" WHERE "A" > :1 ORDER BY "A" FETCH FIRST :2 ROWS ONLY FOR UPDATE"#,
        r#"WITH "X" AS (SELECT 1 FROM DUAL) SELECT 1 FROM "X""#,
        r#"SELECT "A" FROM "T" UNION SELECT "A" FROM "U""#,
        r#"SELECT "A" FROM "T" WHERE "A" IN (SELECT "B" FROM "U")"#,
        r#"SELECT "A" FROM "T" WHERE EXISTS (SELECT 1 FROM "U")"#,
        r#"SELECT "A" FROM "T" JOIN "U" ON "T"."A" = "U"."A""#,
        r#"SELECT "A" FROM "T", "U""#,
        r#"SELECT "A" FROM TABLE("F"())"#,
        r#"SELECT "F"("A") FROM "T""#,
        r#"SELECT "A" FROM "T" WHERE "A" = "F"(:1)"#,
        r#"SELECT "A" FROM "T" ORDER BY "F"("A")"#,
        r#"SELECT "SEQ".NEXTVAL FROM "DUAL""#,
        r#"SELECT NEXTVAL FROM "T""#,
        r#"SELECT "SEQ".CURRVAL FROM "DUAL""#,
        r#"SELECT SYSDATE FROM "DUAL""#,
        r#"SELECT SYSTIMESTAMP FROM "DUAL""#,
        r#"SELECT SESSIONTIMEZONE FROM "DUAL""#,
        r#"SELECT ORA_ROWSCN FROM "T""#,
        r#"SELECT F_AUDIT FROM "T""#,
        r#"SELECT CURRENT_TIMESTAMP FROM "T""#,
        r#"SELECT COUNT(DISTINCT "A") FROM "T""#,
        r#"SELECT COUNT(*) OVER () FROM "T""#,
        r#"SELECT "A" FROM "T" WHERE "A" = 'x'"#,
        r#"SELECT "A" FROM "T" -- c"#,
        r#"SELECT "A" FROM "T" /* c */"#,
        r#"SELECT /*+ PARALLEL(8) */ "A" FROM "T""#,
        r#"SELECT "A" FROM T@LINK"#,
        r#"SELECT "A" FROM "T"@"LINK""#,
        r#"SELECT "A" INTO "B" FROM "T""#,
        r#"SELECT "A" FROM "T" AS OF SCN :1"#,
        "ALTER SESSION SET READ_ONLY = FALSE",
        "PRAGMA query_only = OFF",
    ];
    for sql in samples {
        reject_both(sql);
    }
}

#[test]
fn rejects_unquoted_oracle_identifiers() {
    let samples = [
        r#"SELECT A FROM "T""#,
        r#"SELECT "A" FROM T"#,
        r#"SELECT "A" FROM "T" WHERE A > :1"#,
        r#"SELECT "A" FROM "T" ORDER BY A"#,
        r#"SELECT PKG.F FROM "DUAL""#,
        r#"SELECT "A" FROM "T" WHERE PKG.F = :1"#,
        r#"SELECT "A" FROM "T" ORDER BY PKG.F"#,
        r#"SELECT SYSDATE FROM DUAL"#,
    ];
    for sql in samples {
        reject(sql, SqlDialect::Oracle);
    }
    accept(r#"SELECT "SYSDATE" FROM DUAL"#, SqlDialect::Oracle);
    accept(r#"SELECT "A" FROM "DUAL""#, SqlDialect::Oracle);
    accept(r#"SELECT COUNT(*) FROM "T""#, SqlDialect::Oracle);
    for sql in [
        r#"SELECT "PKG"."F" FROM "T""#,
        r#"SELECT "A" FROM "T" WHERE "PKG"."F" = :1"#,
        r#"SELECT "A" FROM "T" ORDER BY "PKG"."F""#,
        r#"SELECT "T"."A" FROM "T""#,
        r#"SELECT "A" FROM "T" WHERE "T"."A" = :1"#,
        r#"SELECT "A" FROM "T" ORDER BY "T"."A""#,
    ] {
        reject_both(sql);
    }
}

#[test]
fn accepts_generated_shapes() {
    let oracle = [
        r#"SELECT "K", "A", "B" FROM "S"."T" WHERE ("STATUS" IN (:1, :2)) ORDER BY "K" FETCH FIRST :3 ROWS ONLY"#,
        r#"SELECT "K", "A", "B" FROM "S"."T" WHERE ("STATUS" IN (:1, :2)) AND "K" > :3 ORDER BY "K" FETCH FIRST :4 ROWS ONLY"#,
        r#"SELECT "A", "B" FROM "S"."T" WHERE ("STATUS" IN (:1, :2))"#,
        r#"SELECT COUNT(*) FROM "S"."T" WHERE ("STATUS" IN (:1, :2))"#,
    ];
    for sql in oracle {
        accept(sql, SqlDialect::Oracle);
    }
    let sqlite = [
        r#"SELECT "K", "A", "B" FROM "S"."T" WHERE ("STATUS" IN (?, ?)) ORDER BY "K" LIMIT ?"#,
        r#"SELECT "K", "A", "B" FROM "S"."T" WHERE ("STATUS" IN (?, ?)) AND "K" > ? ORDER BY "K" LIMIT ?"#,
        r#"SELECT "A", "B" FROM "S"."T" WHERE ("STATUS" IN (?, ?))"#,
        r#"SELECT COUNT(*) FROM "S"."T" WHERE ("STATUS" IN (?, ?))"#,
    ];
    for sql in sqlite {
        accept(sql, SqlDialect::Sqlite);
    }
}

#[test]
fn accepts_each_where_operator() {
    let oracle = [
        r#"SELECT "A" FROM "T" WHERE ("C" = :1)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" <> :1)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" != :1)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" > :1)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" >= :1)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" < :1)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" <= :1)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" LIKE :1)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" BETWEEN :1 AND :2)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" IN (:1, :2))"#,
        r#"SELECT "A" FROM "T" WHERE ("C" IS NULL)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" IS NOT NULL)"#,
    ];
    for sql in oracle {
        accept(sql, SqlDialect::Oracle);
    }
    let sqlite = [
        r#"SELECT "A" FROM "T" WHERE ("C" = ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" <> ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" != ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" > ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" >= ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" < ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" <= ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" LIKE ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" BETWEEN ? AND ?)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" IN (?, ?))"#,
        r#"SELECT "A" FROM "T" WHERE ("C" IS NULL)"#,
        r#"SELECT "A" FROM "T" WHERE ("C" IS NOT NULL)"#,
    ];
    for sql in sqlite {
        accept(sql, SqlDialect::Sqlite);
    }
}

#[test]
fn accepts_probes_and_dictionary_queries() {
    accept("SELECT 1 FROM DUAL", SqlDialect::Oracle);
    accept("SELECT 1 FROM sqlite_master LIMIT 1", SqlDialect::Sqlite);
    let oracle = [
        r#"SELECT "USERNAME" FROM "USER_USERS""#,
        r#"SELECT "PRIVILEGE" FROM "SESSION_PRIVS""#,
        r#"SELECT "ROLE" FROM "SESSION_ROLES""#,
        r#"SELECT "PRIVILEGE" FROM "USER_TAB_PRIVS_RECD" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#,
        r#"SELECT "ROLE", "PRIVILEGE" FROM "ROLE_TAB_PRIVS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#,
        r#"SELECT "OBJECT_TYPE" FROM "ALL_OBJECTS" WHERE "OWNER" = :1 AND "OBJECT_NAME" = :2"#,
        r#"SELECT "TABLE_OWNER", "TABLE_NAME", "DB_LINK" FROM "ALL_SYNONYMS" WHERE "OWNER" = :1 AND "SYNONYM_NAME" = :2"#,
        r#"SELECT "REFERENCED_OWNER", "REFERENCED_NAME", "REFERENCED_TYPE", "REFERENCED_LINK_NAME" FROM "ALL_DEPENDENCIES" WHERE "OWNER" = :1 AND "NAME" = :2 AND "TYPE" = :3"#,
        r#"SELECT "COLUMN_NAME", "VIRTUAL_COLUMN", "NULLABLE" FROM "ALL_TAB_COLS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#,
        r#"SELECT "COLUMN_NAME", "DATA_TYPE", "DATA_LENGTH", "DATA_PRECISION", "DATA_SCALE", "NULLABLE", "COLUMN_ID" FROM "ALL_TAB_COLUMNS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2 ORDER BY "COLUMN_ID""#,
        r#"SELECT "COLUMN_NAME", "COMMENTS" FROM "ALL_COL_COMMENTS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#,
        r#"SELECT "COMMENTS" FROM "ALL_TAB_COMMENTS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#,
        r#"SELECT "CONSTRAINT_NAME", "COLUMN_NAME", "POSITION" FROM "ALL_CONS_COLUMNS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#,
        r#"SELECT "CONSTRAINT_NAME", "CONSTRAINT_TYPE" FROM "ALL_CONSTRAINTS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2 AND "STATUS" = :3"#,
        r#"SELECT "CONSTRAINT_NAME", "COLUMN_NAME" FROM "ALL_CONS_COLUMNS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2"#,
        r#"SELECT "CONSTRAINT_NAME", "R_OWNER", "R_CONSTRAINT_NAME" FROM "ALL_CONSTRAINTS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2 AND "CONSTRAINT_TYPE" = :3 AND "STATUS" = :4"#,
        r#"SELECT "TABLE_NAME" FROM "ALL_CONSTRAINTS" WHERE "OWNER" = :1 AND "CONSTRAINT_NAME" = :2"#,
        r#"SELECT "COLUMN_NAME", "POSITION" FROM "ALL_CONS_COLUMNS" WHERE "OWNER" = :1 AND "CONSTRAINT_NAME" = :2"#,
        r#"SELECT "OWNER", "INDEX_NAME" FROM "ALL_INDEXES" WHERE "TABLE_OWNER" = :1 AND "TABLE_NAME" = :2 AND "UNIQUENESS" = :3"#,
        r#"SELECT "INDEX_OWNER", "INDEX_NAME", "COLUMN_NAME" FROM "ALL_IND_COLUMNS" WHERE "TABLE_OWNER" = :1 AND "TABLE_NAME" = :2"#,
    ];
    for sql in oracle {
        accept(sql, SqlDialect::Oracle);
    }
    accept(
        r#"SELECT "type", "sql" FROM "sqlite_master" WHERE "name" = ?"#,
        SqlDialect::Sqlite,
    );
}

#[test]
fn builder_output_is_guarded_for_every_operator() {
    for dialect in [SqlDialect::Oracle, SqlDialect::Sqlite] {
        for op in operators() {
            for limit in [None, Some(10)] {
                let spec = spec(op, limit);
                let plan = build::keyset(dialect, &spec, "ORDER_ID").unwrap_or_else(|err| {
                    panic!("{dialect:?} {op:?} limit={limit:?} keyset: {err}")
                });
                assert_guarded(&plan);
                let plan = build::snapshot(dialect, &spec).unwrap_or_else(|err| {
                    panic!("{dialect:?} {op:?} limit={limit:?} snapshot: {err}")
                });
                assert_guarded(&plan);
                let (query, _) = build::count(dialect, &spec).unwrap_or_else(|err| {
                    panic!("{dialect:?} {op:?} limit={limit:?} count: {err}")
                });
                assert_eq!(query.dialect(), dialect);
                assert!(query.sql().contains("COUNT(*)"), "{}", query.sql());
            }
        }
    }
}

#[test]
fn oracle_fk_dictionary_queries_pass_guard() {
    for sql in [
        r#"SELECT "CONSTRAINT_NAME", "R_OWNER", "R_CONSTRAINT_NAME" FROM "ALL_CONSTRAINTS" WHERE "OWNER" = :1 AND "TABLE_NAME" = :2 AND "CONSTRAINT_TYPE" = :3 AND "STATUS" = :4"#,
        r#"SELECT "TABLE_NAME" FROM "ALL_CONSTRAINTS" WHERE "OWNER" = :1 AND "CONSTRAINT_NAME" = :2"#,
        r#"SELECT "COLUMN_NAME", "POSITION" FROM "ALL_CONS_COLUMNS" WHERE "OWNER" = :1 AND "CONSTRAINT_NAME" = :2"#,
    ] {
        accept(sql, SqlDialect::Oracle);
    }
}

#[test]
fn guarded_overlap_select_allows_only_exists_count_template() {
    let allowed = [
        (
            SqlDialect::Sqlite,
            r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID")"#,
        ),
        (
            SqlDialect::Oracle,
            r#"SELECT COUNT(*) FROM "SALES"."ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND "A"."PART" IS NOT NULL AND EXISTS (SELECT 1 FROM "SALES"."INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID" AND "B"."PART" = "A"."PART")"#,
        ),
    ];
    for (dialect, sql) in allowed {
        let guarded = GuardedOverlapSelect::parse(sql, dialect)
            .unwrap_or_else(|err| panic!("{dialect:?}: {err}"));
        assert_eq!(guarded.sql(), sql);
        reject(sql, dialect);
    }
    let rejected = [
        r#"SELECT * FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID")"#,
        r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" = 'x' AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID")"#,
        r#"SELECT COUNT(*) FROM "ORDERS" "A" JOIN "INTERSECTIONS" "B" ON "B"."CROSS_ID" = "A"."CROSS_ID""#,
        r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID") GROUP BY "A"."CROSS_ID""#,
        r#"SELECT COUNT(*), "A"."CROSS_ID" FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID")"#,
        r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = 'KEYCANARY-9a2f')"#,
        "SELECT COUNT(*) FROM \"ORDERS\" \"A\" WHERE \"A\".\"CROSS_ID\" IS NOT NULL AND EXISTS (SELECT 1 FROM \"INTERSECTIONS\" \"B\" WHERE \"B\".\"CROSS_ID\" = \"A\".\"CROSS_ID\") -- note",
        "SELECT COUNT(*) FROM \"ORDERS\" \"A\" WHERE \"A\".\"CROSS_ID\" IS NOT NULL /* c */ AND EXISTS (SELECT 1 FROM \"INTERSECTIONS\" \"B\" WHERE \"B\".\"CROSS_ID\" = \"A\".\"CROSS_ID\")",
        r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID"); SELECT 1"#,
        r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND NOT EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID")"#,
        r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."OTHER")"#,
        r#"SELECT COUNT(*) FROM "ORDERS" "C" WHERE "C"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "C"."CROSS_ID")"#,
        r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 1 FROM "INTERSECTIONS" "D" WHERE "D"."CROSS_ID" = "A"."CROSS_ID")"#,
        r#"SELECT COUNT(*) FROM "ORDERS" "A" WHERE "A"."CROSS_ID" IS NOT NULL AND EXISTS (SELECT 2 FROM "INTERSECTIONS" "B" WHERE "B"."CROSS_ID" = "A"."CROSS_ID")"#,
    ];
    for sql in rejected {
        assert!(
            GuardedOverlapSelect::parse(sql, SqlDialect::Sqlite).is_err(),
            "{sql}"
        );
        assert!(
            GuardedOverlapSelect::parse(sql, SqlDialect::Oracle).is_err(),
            "{sql}"
        );
    }
}

fn assert_guarded(plan: &ReadPlan) {
    assert!(!plan.first.sql().is_empty());
    if let Some(next) = &plan.next {
        assert_ne!(plan.first.sql(), next.sql());
    }
}

fn operators() -> [FilterOp; 11] {
    [
        FilterOp::Eq,
        FilterOp::Ne,
        FilterOp::Gt,
        FilterOp::Gte,
        FilterOp::Lt,
        FilterOp::Lte,
        FilterOp::Like,
        FilterOp::Between,
        FilterOp::In,
        FilterOp::IsNull,
        FilterOp::NotNull,
    ]
}

fn spec(op: FilterOp, limit: Option<u64>) -> SelectSpec {
    let value = match op {
        FilterOp::IsNull | FilterOp::NotNull => None,
        FilterOp::In => Some(toml::Value::Array(vec![
            toml::Value::String("PAID".to_string()),
            toml::Value::String("SHIPPED".to_string()),
        ])),
        FilterOp::Between => Some(toml::Value::Array(vec![
            toml::Value::Integer(1),
            toml::Value::Integer(9),
        ])),
        FilterOp::Eq
        | FilterOp::Ne
        | FilterOp::Gt
        | FilterOp::Gte
        | FilterOp::Lt
        | FilterOp::Lte
        | FilterOp::Like => Some(toml::Value::String("PAID".to_string())),
    };
    SelectSpec {
        table: "S.T".to_string(),
        columns: vec!["ORDER_ID".to_string(), "STATUS".to_string()],
        filters: vec![Filter {
            column: "STATUS".to_string(),
            op,
            value,
        }],
        limit,
        batch_rows: 1000,
        key: None,
        paging: Paging::Keyset,
    }
}
