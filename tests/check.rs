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
    let lines: Vec<_> = stdout.lines().collect();
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
        .args(["--verbose", "check", "--config"])
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
    assert!(
        stdout.contains("company (oracle): FAIL - oracle support is not built in"),
        "{stdout}"
    );
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
        .find(|line| line.starts_with("company (oracle):"))
        .unwrap_or_else(|| panic!("stdout: {stdout}\nstderr: {stderr}"));
    assert!(line.contains("DPI-1047"), "{line}");
    assert!(line.contains("docs/ORACLE_SETUP.md"), "{line}");
    assert!(!stdout.contains("PR2-canary-7f1d9c"), "{stdout}");
    assert!(!stderr.contains("PR2-canary-7f1d9c"), "{stderr}");
}
