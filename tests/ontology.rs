mod common;

use std::path::Path;

fn events_db(dir: &Path) {
    let conn = rusqlite::Connection::open(dir.join("source.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE EVENTS (
            ID INTEGER NOT NULL,
            EVENT_DT DATE NOT NULL,
            QTY INTEGER,
            PRIMARY KEY (EVENT_DT, ID)
        );",
    )
    .unwrap();
}

fn config(dir: &Path) -> std::path::PathBuf {
    common::write_config(
        dir,
        r#"
[sources.sample]
kind = "sqlite"
path = "source.db"
"#,
    )
}

fn pull(dir: &Path, config: &Path, extra: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = common::dipper();
    command.current_dir(dir).args(["--config"]).arg(config);
    command.args(extra);
    command.args(["ontology", "pull", "sample", "--table", "main.EVENTS"]);
    command.assert()
}

#[test]
fn ontology_pull_json_lists_written_files() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    let table = pull(dir.path(), &config, &[]).code(0).get_output().clone();
    let lines: Vec<String> = String::from_utf8(table.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(!lines.is_empty());
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "ontology",
            "pull",
            "sample",
            "--table",
            "main.EVENTS",
            "--format",
            "json",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["command"], "ontology.pull");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["exit_code"], 0);
    assert_eq!(doc["source"], "sample");
    assert!(doc["error"].is_null());
    let files: Vec<String> = serde_json::from_value(doc["files"].clone()).unwrap();
    assert_eq!(files, lines);
    for file in &files {
        assert!(dir.path().join(file).is_file(), "{file}");
    }
}

#[test]
fn pull_without_table_or_dip_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["ontology", "pull", "sample"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("pass --table OWNER.TABLE or configure a dip"),
        "{stderr}"
    );
    assert!(!dir.path().join("ontology").exists());
}

#[test]
fn sqlite_extract_matches_columns_and_primary_key() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    pull(dir.path(), &config, &[]).code(0);
    let text = std::fs::read_to_string(
        dir.path()
            .join("ontology/extracted/sample/main.EVENTS.json"),
    )
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["source"], "sample");
    assert_eq!(doc["owner"], "main");
    assert_eq!(doc["table"], "EVENTS");
    assert_eq!(doc["object_type"], "TABLE");
    assert_eq!(doc["primary_key"], serde_json::json!(["EVENT_DT", "ID"]));
    assert_eq!(doc["time_column"], "EVENT_DT");
    let columns = doc["columns"].as_array().unwrap();
    assert_eq!(columns.len(), 3);
    assert_eq!(columns[0]["name"], "ID");
    assert_eq!(columns[0]["data_type"], "INTEGER");
    assert_eq!(columns[0]["nullable"], false);
    assert_eq!(columns[1]["name"], "EVENT_DT");
    assert_eq!(columns[1]["data_type"], "DATE");
    assert_eq!(columns[1]["nullable"], false);
    assert_eq!(columns[2]["name"], "QTY");
    assert_eq!(columns[2]["nullable"], true);
    for column in columns {
        assert!(column["meaning"].is_null(), "{column}");
        assert!(column["unit"].is_null(), "{column}");
        assert_eq!(column["meaning_status"], "unknown");
        assert_eq!(column["unit_status"], "unknown");
    }
}

#[test]
fn second_pull_preserves_overlay() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    pull(dir.path(), &config, &[]).code(0);
    let overlay_dir = dir.path().join("ontology/overlay/sample");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    let overlay = overlay_dir.join("main.EVENTS.json");
    let body = r#"{
  "version": 1,
  "columns": [
    {
      "name": "QTY",
      "meaning": "how many",
      "meaning_status": "confirmed",
      "unit": "item",
      "unit_status": "confirmed"
    }
  ]
}
"#;
    std::fs::write(&overlay, body).unwrap();
    let before = std::fs::read(&overlay).unwrap();
    pull(dir.path(), &config, &[]).code(0);
    assert_eq!(std::fs::read(&overlay).unwrap(), before);
    let extracted = std::fs::read_to_string(
        dir.path()
            .join("ontology/extracted/sample/main.EVENTS.json"),
    )
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&extracted).unwrap();
    assert!(doc["columns"][2]["meaning"].is_null());
}

#[test]
fn render_writes_markdown_from_the_merged_document() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    pull(dir.path(), &config, &[]).code(0);
    let overlay_dir = dir.path().join("ontology/overlay/sample");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    std::fs::write(
        overlay_dir.join("main.EVENTS.json"),
        r#"{"version":1,"columns":[{"name":"QTY","meaning":"how many","meaning_status":"confirmed","unit":"item","unit_status":"confirmed"}]}"#,
    )
    .unwrap();
    common::dipper()
        .current_dir(dir.path())
        .args(["ontology", "render"])
        .assert()
        .code(0);
    let md = std::fs::read_to_string(dir.path().join("ontology/md/sample/main.EVENTS.md")).unwrap();
    assert!(md.contains("EVENT_DT"), "{md}");
    assert!(md.contains("how many"), "{md}");
    assert!(md.contains("confirmed"), "{md}");
    assert!(md.contains("| ID | INTEGER |"), "{md}");
}

#[test]
fn validate_rejects_a_bad_overlay() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    pull(dir.path(), &config, &[]).code(0);
    let overlay_dir = dir.path().join("ontology/overlay/sample");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    let overlay = overlay_dir.join("main.EVENTS.json");
    std::fs::write(
        &overlay,
        r#"{"version":1,"columns":[{"name":"QTY","meaning_status":"guessed"}]}"#,
    )
    .unwrap();
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["ontology", "validate"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("ontology:"), "{stderr}");

    std::fs::write(
        &overlay,
        r#"{"version":1,"columns":[{"name":"MISSING","meaning":"x","meaning_status":"confirmed"}]}"#,
    )
    .unwrap();
    common::dipper()
        .current_dir(dir.path())
        .args(["ontology", "validate"])
        .assert()
        .code(1);

    std::fs::write(
        &overlay,
        r#"{"version":1,"columns":[{"name":"QTY","meaning":"how many","meaning_status":"confirmed","unit":"item","unit_status":"confirmed"}]}"#,
    )
    .unwrap();
    common::dipper()
        .current_dir(dir.path())
        .args(["ontology", "validate"])
        .assert()
        .code(0);
}

#[test]
fn overlay_rejects_draft_relation_without_evidence_or_unknown_column_or_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    pull(dir.path(), &config, &[]).code(0);
    let overlay_dir = dir.path().join("ontology/overlay/sample");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    let overlay = overlay_dir.join("main.EVENTS.json");
    let bodies = [
        r#"{"version":1,"relations":[{"columns":["QTY"],"target_owner":"main","target_table":"OTHER","target_columns":["QTY"],"status":"draft","evidence":"   "}]}"#,
        r#"{"version":1,"relations":[{"columns":["MISSING"],"target_owner":"main","target_table":"OTHER","target_columns":["QTY"],"status":"draft","evidence":"same name"}]}"#,
        r#"{"version":1,"relations":[{"columns":["QTY"],"target_owner":"main","target_table":"OTHER","target_columns":["QTY"],"status":"draft","evidence":"same name"},{"columns":["qty"],"target_owner":"MAIN","target_table":"other","target_columns":["qty"],"status":"confirmed"}]}"#,
    ];
    for body in bodies {
        std::fs::write(&overlay, body).unwrap();
        common::dipper()
            .current_dir(dir.path())
            .args(["ontology", "validate"])
            .assert()
            .code(1);
    }
}

#[test]
fn overlay_cannot_override_extracted_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    pull(dir.path(), &config, &[]).code(0);
    let overlay_dir = dir.path().join("ontology/overlay/sample");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    std::fs::write(
        overlay_dir.join("main.EVENTS.json"),
        r#"{"version":1,"foreign_keys":[{"name":"fk_0","columns":["QTY"],"target_owner":"main","target_table":"OTHER","target_columns":["QTY"]}]}"#,
    )
    .unwrap();
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["ontology", "validate"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("foreign_keys"), "{stderr}");
}

fn related_db(dir: &Path) {
    let conn = rusqlite::Connection::open(dir.join("source.db")).unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
        CREATE TABLE INTERSECTIONS (
            CROSS_ID TEXT PRIMARY KEY
        );
        CREATE TABLE PARTS (
            CROSS_ID TEXT NOT NULL,
            PART TEXT NOT NULL,
            PRIMARY KEY (CROSS_ID, PART)
        );
        CREATE TABLE ORDERS (
            ID INTEGER PRIMARY KEY,
            CROSS_ID TEXT,
            PART TEXT,
            JYWK_CNT INTEGER,
            VS TEXT,
            XC TEXT,
            FOREIGN KEY (CROSS_ID) REFERENCES INTERSECTIONS (CROSS_ID),
            FOREIGN KEY (CROSS_ID, PART) REFERENCES PARTS (CROSS_ID, PART)
        );
        INSERT INTO INTERSECTIONS (CROSS_ID) VALUES ('KEYCANARY-9a2f');
        INSERT INTO INTERSECTIONS (CROSS_ID) VALUES ('MISS');
        INSERT INTO PARTS (CROSS_ID, PART) VALUES ('KEYCANARY-9a2f', 'A');
        INSERT INTO PARTS (CROSS_ID, PART) VALUES ('OTHER', 'Z');
        INSERT INTO ORDERS (ID, CROSS_ID, PART) VALUES (1, 'KEYCANARY-9a2f', 'A');
        INSERT INTO ORDERS (ID, CROSS_ID, PART) VALUES (2, 'KEYCANARY-9a2f', 'A');
        INSERT INTO ORDERS (ID, CROSS_ID, PART) VALUES (3, 'OTHER', 'Z');
        INSERT INTO ORDERS (ID, CROSS_ID, PART) VALUES (4, NULL, NULL);",
    )
    .unwrap();
}

#[test]
fn sqlite_extract_includes_declared_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    related_db(dir.path());
    let config = config(dir.path());
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["ontology", "pull", "sample", "--table", "ORDERS"])
        .assert()
        .code(0);
    let text = std::fs::read_to_string(
        dir.path()
            .join("ontology/extracted/sample/main.ORDERS.json"),
    )
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(doc.get("relations").is_none(), "{doc}");
    let keys = doc["foreign_keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert_eq!(keys[0]["name"], "fk_0");
    assert_eq!(keys[0]["columns"], serde_json::json!(["CROSS_ID", "PART"]));
    assert_eq!(keys[0]["target_owner"], "main");
    assert_eq!(keys[0]["target_table"], "PARTS");
    assert_eq!(
        keys[0]["target_columns"],
        serde_json::json!(["CROSS_ID", "PART"])
    );
    assert_eq!(keys[1]["name"], "fk_1");
    assert_eq!(keys[1]["columns"], serde_json::json!(["CROSS_ID"]));
    assert_eq!(keys[1]["target_owner"], "main");
    assert_eq!(keys[1]["target_table"], "INTERSECTIONS");
    assert_eq!(keys[1]["target_columns"], serde_json::json!(["CROSS_ID"]));
    for column in doc["columns"].as_array().unwrap() {
        assert!(column["meaning"].is_null(), "{column}");
        assert!(column["unit"].is_null(), "{column}");
        assert_eq!(column["meaning_status"], "unknown");
        assert_eq!(column["unit_status"], "unknown");
    }
    let names: Vec<&str> = doc["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|column| column["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"JYWK_CNT"), "{names:?}");
    assert!(names.contains(&"VS"), "{names:?}");
    assert!(names.contains(&"XC"), "{names:?}");
}

#[test]
fn render_outputs_obsidian_wikilinks_and_index() {
    let dir = tempfile::tempdir().unwrap();
    related_db(dir.path());
    let config = config(dir.path());
    for table in ["ORDERS", "INTERSECTIONS"] {
        common::dipper()
            .current_dir(dir.path())
            .args(["--config"])
            .arg(&config)
            .args(["ontology", "pull", "sample", "--table", table])
            .assert()
            .code(0);
    }
    let overlay_dir = dir.path().join("ontology/overlay/sample");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    std::fs::write(
        overlay_dir.join("main.ORDERS.json"),
        r#"{
  "version": 1,
  "relations": [
    {
      "columns": ["CROSS_ID"],
      "target_owner": "main",
      "target_table": "INTERSECTIONS",
      "target_columns": ["CROSS_ID"],
      "status": "draft",
      "evidence": "same column name",
      "overlap": {
        "source_non_null_rows": 1200,
        "target_non_null_rows": 80,
        "matched_rows": 1195,
        "match_ratio": 0.9958333333333333
      }
    },
    {
      "columns": ["PART"],
      "target_owner": "main",
      "target_table": "INTERSECTIONS",
      "target_columns": ["PART"],
      "status": "confirmed",
      "evidence": "confirmed by the user"
    }
  ]
}"#,
    )
    .unwrap();
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["ontology", "render"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("_index.md"), "{stdout}");
    let md = std::fs::read_to_string(dir.path().join("ontology/md/sample/main.ORDERS.md")).unwrap();
    assert!(md.contains("| ID | INTEGER |"), "{md}");
    assert!(
        md.contains("- `CROSS_ID, PART` -> [[main.PARTS]] (`CROSS_ID, PART`) (fk_0)"),
        "{md}"
    );
    assert!(
        md.contains("- `CROSS_ID` -> [[main.INTERSECTIONS]] (`CROSS_ID`) (fk_1)"),
        "{md}"
    );
    assert!(
        md.contains("- [draft] `CROSS_ID` -> [[main.INTERSECTIONS]] (`CROSS_ID`) — same column name (overlap: 1195/1200, 99.6%)"),
        "{md}"
    );
    assert!(
        md.contains(
            "- [confirmed] `PART` -> [[main.INTERSECTIONS]] (`PART`) — confirmed by the user"
        ),
        "{md}"
    );
    let confirmed = md
        .lines()
        .find(|line| line.contains("[confirmed]"))
        .unwrap();
    assert!(!confirmed.contains("overlap"), "{confirmed}");
    let index = std::fs::read_to_string(dir.path().join("ontology/md/sample/_index.md")).unwrap();
    assert!(index.contains("[[main.INTERSECTIONS]]"), "{index}");
    assert!(index.contains("[[main.ORDERS]]"), "{index}");
    let inter = index.find("[[main.INTERSECTIONS]]").unwrap();
    let orders = index.find("[[main.ORDERS]]").unwrap();
    assert!(inter < orders, "{index}");
}

#[test]
fn check_relation_json_returns_counts_without_row_values_or_secrets() {
    let dir = tempfile::tempdir().unwrap();
    related_db(dir.path());
    let config = config(dir.path());
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args([
            "ontology",
            "check-relation",
            "sample",
            "--from-table",
            "ORDERS",
            "--from-column",
            "CROSS_ID",
            "--to-table",
            "INTERSECTIONS",
            "--to-column",
            "CROSS_ID",
            "--format",
            "json",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stdout.contains("KEYCANARY-9a2f"), "{stdout}");
    assert!(!stderr.contains("KEYCANARY-9a2f"), "{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["command"], "ontology.check_relation");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["exit_code"], 0);
    assert_eq!(doc["source"], "sample");
    assert_eq!(doc["from"]["owner"], "main");
    assert_eq!(doc["from"]["table"], "ORDERS");
    assert_eq!(doc["from"]["columns"], serde_json::json!(["CROSS_ID"]));
    assert_eq!(doc["to"]["table"], "INTERSECTIONS");
    assert_eq!(doc["source_non_null_rows"], 3);
    assert_eq!(doc["target_non_null_rows"], 2);
    assert_eq!(doc["matched_rows"], 2);
    let ratio = doc["match_ratio"].as_f64().unwrap();
    assert!((ratio - (2.0 / 3.0)).abs() < 1e-12, "{ratio}");
    assert!(doc["error"].is_null());
    let sql_lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("sql: "))
        .collect();
    assert!(
        sql_lines
            .iter()
            .any(|line| line.contains("COUNT(*)") && line.contains("EXISTS")),
        "{sql_lines:?}"
    );
    assert!(
        sql_lines.iter().all(|line| {
            let sql = line.trim_start_matches("sql: ");
            sql == "BEGIN"
                || sql == "ROLLBACK"
                || sql.starts_with("SELECT COUNT(*)")
                || sql.starts_with("PRAGMA query_only")
        }),
        "{sql_lines:?}"
    );

    let empty = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "ontology",
            "check-relation",
            "sample",
            "--from-table",
            "ORDERS",
            "--from-column",
            "VS",
            "--to-table",
            "INTERSECTIONS",
            "--to-column",
            "CROSS_ID",
            "--format",
            "json",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let empty_doc: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(empty_doc["source_non_null_rows"], 0);
    assert_eq!(empty_doc["match_ratio"], 0.0);
}

#[test]
fn check_relation_rejects_mismatched_column_arity_or_missing_column() {
    let dir = tempfile::tempdir().unwrap();
    related_db(dir.path());
    let config = config(dir.path());
    let mismatch = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "ontology",
            "check-relation",
            "sample",
            "--from-table",
            "ORDERS",
            "--from-column",
            "CROSS_ID",
            "--from-column",
            "PART",
            "--to-table",
            "INTERSECTIONS",
            "--to-column",
            "CROSS_ID",
            "--format",
            "json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let mismatch_doc: serde_json::Value = serde_json::from_slice(&mismatch.stdout).unwrap();
    assert_eq!(mismatch_doc["ok"], false);
    assert_eq!(mismatch_doc["exit_code"], 1);
    assert!(mismatch_doc["error"]["message"].is_string());

    let missing = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "ontology",
            "check-relation",
            "sample",
            "--from-table",
            "ORDERS",
            "--from-column",
            "NO_SUCH_COLUMN",
            "--to-table",
            "INTERSECTIONS",
            "--to-column",
            "CROSS_ID",
            "--format",
            "json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8(missing.stdout).unwrap();
    let stderr = String::from_utf8(missing.stderr).unwrap();
    assert!(!stdout.contains("KEYCANARY-9a2f"), "{stdout}");
    assert!(!stderr.contains("KEYCANARY-9a2f"), "{stderr}");
    let missing_doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(missing_doc["ok"], false);
    assert_eq!(missing_doc["command"], "ontology.check_relation");
}

#[test]
fn check_relation_hides_oracle_credentials_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::write_config(
        dir.path(),
        r#"
[sources.company]
kind = "oracle"
connect = "127.0.0.1:1521/NOPE"
user = "reader"
password = "${DIPPER_PR3B_PASSWORD}"
"#,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .env("DIPPER_PR3B_PASSWORD", "PR2-canary-7f1d9c")
        .args(["--config"])
        .arg(&config)
        .args([
            "ontology",
            "check-relation",
            "company",
            "--from-table",
            "SALES.ORDERS",
            "--from-column",
            "ORDER_ID",
            "--to-table",
            "SALES.CUSTOMERS",
            "--to-column",
            "CUSTOMER_ID",
            "--format",
            "json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["command"], "ontology.check_relation");
    for text in [&stdout, &stderr] {
        assert!(!text.contains("PR2-canary-7f1d9c"), "{text}");
        assert!(!text.contains("reader"), "{text}");
        assert!(!text.contains("127.0.0.1:1521/NOPE"), "{text}");
    }
}

#[test]
fn dictionary_sql_is_logged_only_with_log_sql() {
    let dir = tempfile::tempdir().unwrap();
    events_db(dir.path());
    let config = config(dir.path());
    let quiet = pull(dir.path(), &config, &[]).code(0).get_output().clone();
    let quiet_err = String::from_utf8(quiet.stderr).unwrap();
    assert!(!quiet_err.contains("sql:"), "{quiet_err}");

    let logged = pull(dir.path(), &config, &["--log-sql"])
        .code(0)
        .get_output()
        .clone();
    let stderr = String::from_utf8(logged.stderr).unwrap();
    assert!(stderr.contains("sql: "), "{stderr}");
    assert!(stderr.contains("sqlite_master"), "{stderr}");
    assert!(stderr.contains("PRAGMA table_xinfo"), "{stderr}");
    assert!(
        stderr
            .lines()
            .filter(|line| line.starts_with("sql: "))
            .any(|line| line.contains('?')),
        "{stderr}"
    );
}
