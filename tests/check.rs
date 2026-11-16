mod common;

use std::path::Path;

fn sqlite_sources(dir: &Path, names: &[(&str, &str)]) -> std::path::PathBuf {
    let mut text = String::new();
    for (name, file) in names {
        text.push_str(&format!(
            "[sources.{name}]\nkind = \"sqlite\"\npath = \"{file}\"\n\n"
        ));
    }
    common::write_config(dir, &text)
}

fn output_text(output: &std::process::Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn two_sqlite_sources_are_ok() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    common::make_sqlite(dir.path(), "archive.db", &["CUSTOMERS"]);
    let config = sqlite_sources(
        dir.path(),
        &[("sample", "sample.db"), ("archive", "archive.db")],
    );
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&config)
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    let lines: Vec<_> = stdout
        .lines()
        .filter(|line| line.contains("(sqlite)") || line.ends_with("sources OK"))
        .collect();
    assert_eq!(
        lines,
        [
            "archive (sqlite): OK",
            "sample (sqlite): OK",
            "2 of 2 sources OK",
        ]
    );

    let output = common::dipper()
        .args(["--config"])
        .arg(&config)
        .arg("check")
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    assert_eq!(stdout.lines().last(), Some("2 of 2 sources OK"));
}

#[test]
fn missing_sqlite_file_fails_and_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let missing = dir.path().join("missing.db");
    let config = sqlite_sources(
        dir.path(),
        &[("sample", "sample.db"), ("missing", "missing.db")],
    );
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&config)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = output_text(&output);
    let missing_line = stdout
        .lines()
        .find(|line| line.starts_with("missing (sqlite):"))
        .unwrap_or_else(|| panic!("stdout: {stdout}"));
    assert!(missing_line.contains("FAIL -"), "{missing_line}");
    assert!(
        missing_line.contains(&missing.display().to_string()),
        "{missing_line}"
    );
    assert_eq!(stdout.lines().last(), Some("1 of 2 sources OK"));
    assert!(stderr.contains("check failed"), "{stderr}");
    assert!(!missing.exists());
}

#[test]
fn non_sqlite_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notadb.db"), b"not a database").unwrap();
    let config = sqlite_sources(dir.path(), &[("sample", "notadb.db")]);
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&config)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("sample (sqlite):"))
        .unwrap_or_else(|| panic!("stdout: {stdout}"));
    assert!(line.contains("FAIL -"), "{line}");
}

#[test]
fn check_does_not_modify_sqlite_source() {
    let dir = tempfile::tempdir().unwrap();
    let db = common::make_sqlite(dir.path(), "sample.db", &["ORDERS", "CUSTOMERS"]);
    let config = sqlite_sources(dir.path(), &[("sample", "sample.db")]);
    let before_hash = common::sha256_file(&db);
    let before_list = common::list_dir(dir.path());
    common::dipper()
        .args(["check", "--config"])
        .arg(&config)
        .assert()
        .code(0);
    assert_eq!(common::sha256_file(&db), before_hash);
    assert_eq!(common::list_dir(dir.path()), before_list);
    for name in common::list_dir(dir.path()) {
        assert!(!name.contains("-journal"), "{name}");
        assert!(!name.ends_with("-wal"), "{name}");
        assert!(!name.ends_with("-shm"), "{name}");
        assert_ne!(name, ".dipper");
    }
}

fn company_config(dir: &Path) -> std::path::PathBuf {
    common::make_sqlite(dir, "sample.db", &["ORDERS"]);
    common::write_config(
        dir,
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[sources.company]
kind = "oracle"
connect = "127.0.0.1:1/NOPE"
user = "${DIPPER_TEST_ORA_USER}"
password = "${DIPPER_TEST_ORA_PASSWORD}"
"#,
    )
}

fn verbose_company_check(dir: &Path) -> std::process::Output {
    let config = company_config(dir);
    common::dipper()
        .env("DIPPER_TEST_ORA_USER", "scott")
        .env("DIPPER_TEST_ORA_PASSWORD", "PR2-canary-7f1d9c")
        .args(["--verbose", "--log-sql", "check", "--config"])
        .arg(&config)
        .output()
        .unwrap()
}

#[test]
fn verbose_check_does_not_print_password() {
    let dir = tempfile::tempdir().unwrap();
    let output = verbose_company_check(dir.path());
    assert_eq!(output.status.code(), Some(1));
    let (stdout, stderr) = output_text(&output);
    assert!(
        !stdout.contains("PR2-canary-7f1d9c"),
        "stdout leaked password:\n{stdout}"
    );
    assert!(
        !stderr.contains("PR2-canary-7f1d9c"),
        "stderr leaked password:\n{stderr}"
    );
}

#[cfg(not(feature = "oracle"))]
#[test]
fn oracle_source_without_feature_is_not_built_in() {
    let dir = tempfile::tempdir().unwrap();
    let output = verbose_company_check(dir.path());
    assert_eq!(output.status.code(), Some(1));
    let (stdout, _) = output_text(&output);
    assert!(stdout.contains("env oracle_feature: fail"), "{stdout}");
}

#[cfg(feature = "oracle")]
#[test]
fn oracle_source_without_client_reports_dpi_1047() {
    let dir = tempfile::tempdir().unwrap();
    let output = verbose_company_check(dir.path());
    assert_eq!(output.status.code(), Some(1));
    let (stdout, stderr) = output_text(&output);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("env instant_client:"))
        .unwrap_or_else(|| panic!("stdout: {stdout}\nstderr: {stderr}"));
    assert!(line.contains("DPI-1047"), "{line}");
    assert!(line.contains("docs/ORACLE_SETUP.md"), "{line}");
    assert!(!stdout.contains("PR2-canary-7f1d9c"), "{stdout}");
    assert!(!stderr.contains("PR2-canary-7f1d9c"), "{stderr}");
}

#[test]
fn check_env_only_fails_when_config_env_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.db");
    let config = common::write_config(
        dir.path(),
        r#"
[sources.sample]
kind = "sqlite"
path = "missing.db"
password_note = "${DIPPER_TEST_MISSING}"
"#,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .env_remove("DIPPER_TEST_MISSING")
        .env("DIPPER_TEST_OTHER_VALUE", "SHOULD_NOT_PRINT")
        .args(["--config"])
        .arg(&config)
        .args(["check", "--env-only", "--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["command"], "check");
    assert_eq!(doc["sources"].as_array().unwrap().len(), 0);
    let config_item = doc["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "config")
        .unwrap();
    assert_eq!(config_item["status"], "fail");
    assert!(config_item["detail"]
        .as_str()
        .unwrap()
        .contains("DIPPER_TEST_MISSING"));
    assert!(config_item["fix"]
        .as_str()
        .unwrap()
        .contains("DIPPER_TEST_MISSING"));
    assert!(!stdout.contains("SHOULD_NOT_PRINT"), "{stdout}");
    assert!(!stderr.contains("SHOULD_NOT_PRINT"), "{stderr}");
    assert!(!missing.exists());

    let db = common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let before = common::sha256_file(&db);
    let valid = sqlite_sources(dir.path(), &[("sample", "sample.db")]);
    let output = common::dipper()
        .args(["--log-sql", "check", "--env-only", "--config"])
        .arg(&valid)
        .args(["--format", "json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, stderr) = output_text(&output);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(doc["sources"].as_array().unwrap().is_empty(), "{stdout}");
    assert!(!stderr.contains("sql:"), "{stderr}");
    assert_eq!(common::sha256_file(&db), before);
}

#[test]
fn check_env_reports_oracle_feature_and_executable_path() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let config = common::write_config(
        dir.path(),
        r#"
[sources.company]
kind = "oracle"
connect = "127.0.0.1:1/NOPE"
user = "reader"
password = "${DIPPER_TEST_ORA_PASSWORD}"
"#,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .env("DIPPER_TEST_ORA_PASSWORD", "not-a-real-secret")
        .env("PATH", "")
        .args(["--config"])
        .arg(&config)
        .args(["check", "--env-only"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("env executable: warn"), "{stdout}");
    assert!(stdout.contains("fix:"), "{stdout}");
    if cfg!(feature = "oracle") {
        assert!(stdout.contains("env oracle_feature: ok"), "{stdout}");
    } else {
        assert!(stdout.contains("env oracle_feature: fail"), "{stdout}");
    }
    assert!(!stdout.contains("not-a-real-secret"), "{stdout}");

    let dipper = std::path::Path::new(env!("CARGO_BIN_EXE_dipper"));
    let folder = dipper.parent().unwrap();
    let output = common::dipper()
        .current_dir(dir.path())
        .env("DIPPER_TEST_ORA_PASSWORD", "not-a-real-secret")
        .env("PATH", folder)
        .args(["--config"])
        .arg(&config)
        .args(["check", "--env-only"])
        .assert()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("env executable: ok"), "{stdout}");
}

#[test]
fn check_format_json_matches_the_env_schema() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let config = common::write_config(
        dir.path(),
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"
"#,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["check", "--format", "json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["command"], "check");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["exit_code"], 0);
    assert!(doc["error"].is_null());
    let names: Vec<&str> = doc["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["executable", "config", "oracle_feature", "instant_client"]
    );
    for item in doc["env"].as_array().unwrap() {
        assert!(item["status"].is_string());
        assert!(item["detail"].is_string());
        assert!(item.get("fix").is_some());
    }
    assert_eq!(doc["sources"][0]["name"], "sample");
    assert_eq!(doc["sources"][0]["status"], "ok");
    assert!(doc["sources"][0]["error_code"].is_null());
    assert!(doc["sources"][0]["findings"].is_array());
}

fn view_db(dir: &Path) {
    let conn = rusqlite::Connection::open(dir.join("sample.db")).unwrap();
    conn.execute("CREATE TABLE ORDERS (ORDER_ID INTEGER)", [])
        .unwrap();
    conn.execute("CREATE VIEW V_ORDERS AS SELECT ORDER_ID FROM ORDERS", [])
        .unwrap();
}

fn view_config(dir: &Path, safety: bool) -> std::path::PathBuf {
    view_db(dir);
    let safety = if safety {
        "[safety]\nstrict_readonly = true\n\n"
    } else {
        ""
    };
    common::write_config(
        dir,
        &format!(
            r#"
{safety}[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "V_ORDERS"
columns = ["ORDER_ID"]
paging = "snapshot"
"#
        ),
    )
}

#[test]
fn sqlite_view_warns_and_strict_fails() {
    let dir = tempfile::tempdir().unwrap();
    let config = view_config(dir.path(), false);
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&config)
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    assert!(stdout.contains("sample (sqlite): OK"), "{stdout}");
    assert!(
        stdout.contains("  warning: orders: target is a view (V_ORDERS)"),
        "{stdout}"
    );
    assert_eq!(stdout.lines().last(), Some("1 of 1 sources OK"));

    let strict = common::dipper()
        .args(["--strict", "check", "--config"])
        .arg(&config)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = output_text(&strict);
    assert!(
        stdout.contains("sample (sqlite): FAIL - strict: 1 warning(s)"),
        "{stdout}"
    );
    assert!(stderr.contains("check failed"), "{stderr}");

    let dir = tempfile::tempdir().unwrap();
    let config = view_config(dir.path(), true);
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&config)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    assert!(stdout.contains("FAIL - strict: 1 warning(s)"), "{stdout}");
}

#[test]
fn missing_table_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let config = common::write_config(
        dir.path(),
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "MISSING"
columns = ["ORDER_ID"]
"#,
    );
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&config)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = output_text(&output);
    assert!(stdout.contains("  refused: orders:"), "{stdout}");
    assert!(stderr.contains("check failed"), "{stderr}");
}

#[test]
fn check_json_reports_a_toml_parse_failure() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::write_config(
        dir.path(),
        "[sources.s]\nkind = \"sqlite\"\npath = \"src.db\"\nunknown_key = [\n",
    );
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&config)
        .args(["--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["exit_code"], 1);
    assert_eq!(doc["error"]["code"], "config");
    assert!(doc["sources"].as_array().unwrap().is_empty());
}

#[test]
fn strict_readonly_does_not_fail_a_healthy_source_for_a_path_warning() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let config = common::write_config(
        dir.path(),
        r#"
[safety]
strict_readonly = true

[sources.sample]
kind = "sqlite"
path = "sample.db"
"#,
    );
    let output = common::dipper()
        .env("PATH", "")
        .args(["check", "--config"])
        .arg(&config)
        .args(["--format", "json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["exit_code"], 0);
    assert!(doc["error"].is_null());
    assert_eq!(doc["sources"][0]["status"], "ok");
    let executable = doc["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "executable")
        .unwrap();
    assert_eq!(executable["status"], "warn");

    let strict = common::dipper()
        .env("PATH", "")
        .args(["--strict", "check", "--config"])
        .arg(&config)
        .args(["--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = output_text(&strict);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["exit_code"], 1);
    assert_eq!(doc["sources"][0]["status"], "ok");
    assert!(
        !stdout.contains("0 of 1 sources failed"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("--strict") || stdout.contains("--strict"),
        "{stdout}\n{stderr}"
    );
}

#[test]
fn oracle_kind_without_spaces_is_detected_and_a_comment_is_not() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let nospace = common::write_config(
        dir.path(),
        r#"
[sources.company]
kind="oracle"
connect="127.0.0.1:1/NOPE"
user="APPUSER"
password="${DIPPER_TEST_ORA_PASSWORD}"
"#,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .env("DIPPER_TEST_ORA_PASSWORD", "not-a-real-secret")
        .args(["--config"])
        .arg(&nospace)
        .args(["check", "--env-only", "--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let oracle = env_item(&doc, "oracle_feature");
    let client = env_item(&doc, "instant_client");
    if cfg!(feature = "oracle") {
        assert_eq!(oracle["status"], "ok");
        assert_eq!(client["status"], "fail");
    } else {
        assert_eq!(oracle["status"], "fail");
    }
    assert!(!stdout.contains("not-a-real-secret"), "{stdout}");

    let comment = common::write_config(
        dir.path(),
        "# kind = \"oracle\"\n[sources.sample]\nkind = \"sqlite\"\npath = \"sample.db\"\n",
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&comment)
        .args(["check", "--env-only", "--format", "json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = output_text(&output);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let oracle = env_item(&doc, "oracle_feature");
    assert_eq!(oracle["status"], "ok");
    if !cfg!(feature = "oracle") {
        assert_eq!(oracle["detail"], "no Oracle source is configured");
    }
    let client = env_item(&doc, "instant_client");
    assert_ne!(client["status"], "fail");
}

fn env_item<'a>(doc: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    doc["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("missing {name} in {doc}"))
}

#[test]
fn check_env_only_opens_no_source_when_config_is_valid() {
    let dir = tempfile::tempdir().unwrap();
    let db = common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let before = common::sha256_file(&db);
    let config = sqlite_sources(dir.path(), &[("sample", "sample.db")]);
    let output = common::dipper()
        .args(["--log-sql", "check", "--env-only", "--config"])
        .arg(&config)
        .args(["--format", "json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, stderr) = output_text(&output);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(doc["ok"], true);
    assert!(doc["sources"].as_array().unwrap().is_empty());
    assert!(!stderr.contains("sql:"), "{stderr}");
    assert_eq!(common::sha256_file(&db), before);
}

#[test]
fn log_sql_prints_statements_without_bind_values() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let config = sqlite_sources(dir.path(), &[("sample", "sample.db")]);
    let output = common::dipper()
        .args(["--log-sql", "check", "--config"])
        .arg(&config)
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = output_text(&output);
    assert!(stderr.contains("sql: PRAGMA query_only = ON"), "{stderr}");
    assert!(
        stderr.contains("sql: SELECT 1 FROM sqlite_master LIMIT 1"),
        "{stderr}"
    );
}
