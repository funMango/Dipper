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
