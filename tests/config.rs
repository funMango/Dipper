mod common;

use dipper::config::{load_with_env, parse_memory_limit, FilterOp, SourceConfig};

const EXAMPLE: &str = r#"
store = ".dipper/store.duckdb"

[performance]
batch_rows = 10000
memory_limit = "2GB"
temp_directory = ".dipper/tmp"
progress_every_rows = 1000000

[sources.sample]
kind = "sqlite"
path = "sample.db"

[sources.archive]
kind = "sqlite"
path = "archive.db"

[sources.company]
kind = "oracle"
connect = "127.0.0.1:1/NOPE"
user = "${DIPPER_TEST_ORA_USER}"
password = "${DIPPER_TEST_ORA_PASSWORD}"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID", "CUSTOMER_ID", "STATUS", "AMOUNT"]
filters = [{ column = "STATUS", op = "in", value = ["PAID", "SHIPPED"] }]
limit = 1000

[[dips]]
name = "customers"
source = "archive"
table = "main.CUSTOMERS"
columns = ["CUSTOMER_ID", "REGION"]
count_first = true
"#;

fn example_env(key: &str) -> Option<String> {
    match key {
        "DIPPER_TEST_ORA_USER" => Some("scott".to_string()),
        "DIPPER_TEST_ORA_PASSWORD" => Some("PR2-loader-secret".to_string()),
        _ => None,
    }
}

#[test]
fn example_loads_and_omitted_fields_use_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_config(dir.path(), EXAMPLE);
    let config = load_with_env(&path, &example_env).unwrap();

    assert_eq!(config.file, path);
    assert_eq!(config.store, dir.path().join(".dipper/store.duckdb"));
    assert_eq!(config.performance.batch_rows, 10_000);
    assert_eq!(config.performance.memory_limit, "2GB");
    assert_eq!(
        config.performance.temp_directory,
        dir.path().join(".dipper/tmp")
    );
    assert_eq!(config.performance.progress_every_rows, 1_000_000);

    match config.sources.get("sample") {
        Some(SourceConfig::Sqlite { path }) => assert_eq!(path, &dir.path().join("sample.db")),
        other => panic!("sample source: {other:?}"),
    }
    match config.sources.get("archive") {
        Some(SourceConfig::Sqlite { path }) => assert_eq!(path, &dir.path().join("archive.db")),
        other => panic!("archive source: {other:?}"),
    }
    match config.sources.get("company") {
        Some(SourceConfig::Oracle {
            connect,
            user,
            password,
        }) => {
            assert_eq!(connect, "127.0.0.1:1/NOPE");
            assert_eq!(user, "scott");
            assert_eq!(password.expose(), "PR2-loader-secret");
        }
        other => panic!("company source: {other:?}"),
    }

    assert_eq!(config.dips.len(), 2);
    assert_eq!(config.dips[0].name, "orders");
    assert_eq!(config.dips[0].source, "sample");
    assert_eq!(config.dips[0].table, "ORDERS");
    assert_eq!(
        config.dips[0].columns,
        ["ORDER_ID", "CUSTOMER_ID", "STATUS", "AMOUNT"]
    );
    assert_eq!(config.dips[0].limit, Some(1000));
    assert!(!config.dips[0].count_first);
    assert_eq!(config.dips[0].filters.len(), 1);
    assert_eq!(config.dips[0].filters[0].column, "STATUS");
    assert_eq!(config.dips[0].filters[0].op, FilterOp::In);
    match &config.dips[0].filters[0].value {
        Some(toml::Value::Array(items)) => {
            let texts: Vec<_> = items.iter().map(|value| value.as_str().unwrap()).collect();
            assert_eq!(texts, ["PAID", "SHIPPED"]);
        }
        other => panic!("filter value: {other:?}"),
    }

    assert_eq!(config.dips[1].name, "customers");
    assert_eq!(config.dips[1].source, "archive");
    assert_eq!(config.dips[1].table, "main.CUSTOMERS");
    assert_eq!(config.dips[1].columns, ["CUSTOMER_ID", "REGION"]);
    assert!(config.dips[1].count_first);
    assert!(config.dips[1].filters.is_empty());
    assert_eq!(config.dips[1].limit, None);

    let minimal = r#"
[sources.only]
kind = "sqlite"
path = "only.db"

[[dips]]
name = "orders"
source = "only"
table = "ORDERS"
columns = ["ORDER_ID"]
"#;
    let minimal_path = common::write_config(dir.path(), minimal);
    let config = load_with_env(&minimal_path, &|_| None).unwrap();
    assert_eq!(config.store, dir.path().join(".dipper/store.duckdb"));
    assert_eq!(config.performance.batch_rows, 10_000);
    assert_eq!(config.performance.memory_limit, "2GB");
    assert_eq!(
        config.performance.temp_directory,
        dir.path().join(".dipper/tmp")
    );
    assert_eq!(config.performance.progress_every_rows, 1_000_000);
    assert!(!config.dips[0].count_first);
    match config.sources.get("only") {
        Some(SourceConfig::Sqlite { path }) => assert_eq!(path, &dir.path().join("only.db")),
        other => panic!("only source: {other:?}"),
    }
}

#[test]
fn substituted_fields_do_not_appear_in_debug() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_config(
        dir.path(),
        r#"
[sources.sample]
kind = "sqlite"
path = "${DIPPER_TEST_PATH}"

[sources.company]
kind = "oracle"
connect = "host/${DIPPER_TEST_CONNECT}"
user = "${DIPPER_TEST_ORA_USER}"
password = "${DIPPER_TEST_ORA_PASSWORD}"
"#,
    );
    let config = load_with_env(&path, &|key| match key {
        "DIPPER_TEST_PATH" => Some("nested/sample.db".to_string()),
        "DIPPER_TEST_CONNECT" => Some("db.example:1521/SVC".to_string()),
        "DIPPER_TEST_ORA_USER" => Some("scott".to_string()),
        "DIPPER_TEST_ORA_PASSWORD" => Some("PR2-loader-secret".to_string()),
        _ => None,
    })
    .unwrap();

    match config.sources.get("sample") {
        Some(SourceConfig::Sqlite { path }) => {
            assert_eq!(path, &dir.path().join("nested/sample.db"));
        }
        other => panic!("sample: {other:?}"),
    }
    match config.sources.get("company") {
        Some(SourceConfig::Oracle {
            connect,
            user,
            password,
        }) => {
            assert_eq!(connect, "host/db.example:1521/SVC");
            assert_eq!(user, "scott");
            assert_eq!(password.expose(), "PR2-loader-secret");
        }
        other => panic!("company: {other:?}"),
    }
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("PR2-loader-secret"), "{rendered}");
}

#[test]
fn validation_errors_include_field_paths() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
columns = ["ORDER_ID"]
"#,
            &["dips[0]", "table"][..],
        ),
        (
            r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "nope"
table = "ORDERS"
columns = ["ORDER_ID"]
"#,
            &["dips[0].source", "nope"],
        ),
        (
            r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]

[[dips]]
name = "Orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]
"#,
            &["dips[1].name", "duplicate"],
        ),
        (
            r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS; DROP"
columns = ["ORDER_ID"]
"#,
            &["dips[0].table", "invalid identifier"],
        ),
        (
            r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]
filters = [{ column = "STATUS", op = "contains", value = "X" }]
"#,
            &["dips[0].filters[0].op", "contains"],
        ),
        (
            r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[performance]
memory_limit = "2 gigs"
"#,
            &["performance.memory_limit"],
        ),
        (
            r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
colums = ["ORDER_ID"]
"#,
            &["dips[0]", "colums"],
        ),
        (
            r#"
[sources.company]
kind = "oracle"
connect = "127.0.0.1:1/NOPE"
user = "scott"
password = "${DIPPER_TEST_UNSET_VAR}"
"#,
            &["sources.company.password", "DIPPER_TEST_UNSET_VAR"],
        ),
        (
            r#"
[sources.company]
kind = "oracle"
connect = "127.0.0.1:1/NOPE"
user = "scott"
password = "PR2-canary-literal"
"#,
            &["sources.company.password", "${"],
        ),
    ];

    for (toml_text, needles) in cases {
        let path = common::write_config(dir.path(), toml_text);
        let err = load_with_env(&path, &|_| None).unwrap_err();
        let rendered = err.to_string();
        for needle in needles {
            assert!(rendered.contains(needle), "missing {needle} in {rendered}");
        }
        assert!(!rendered.contains("PR2-canary-literal"), "{rendered}");
    }

    let broken = "password = \"PR2-canary-syntax\n";
    let path = common::write_config(dir.path(), broken);
    let err = load_with_env(&path, &|_| None).unwrap_err();
    let rendered = err.to_string();
    assert!(!rendered.contains("PR2-canary-syntax"), "{rendered}");
}

#[test]
fn parse_memory_limit_units() {
    assert_eq!(parse_memory_limit("2GB").unwrap(), 2_000_000_000);
    assert_eq!(parse_memory_limit("512 MB").unwrap(), 512_000_000);
    assert_eq!(parse_memory_limit("1.5GiB").unwrap(), 1_610_612_736);
    assert_eq!(parse_memory_limit("1gb").unwrap(), 1_000_000_000);
    for bad in ["2 gigs", "0GB", "GB", "-1GB", ""] {
        assert!(parse_memory_limit(bad).is_err(), "{bad}");
    }
}

fn run_check(toml_text: &str) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_config(dir.path(), toml_text);
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn assert_check_error(toml_text: &str, needles: &[&str]) {
    let (code, stdout, stderr) = run_check(toml_text);
    assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
    for needle in needles {
        assert!(
            stderr.contains(needle),
            "missing {needle} in stderr:\n{stderr}"
        );
    }
}

#[test]
fn cli_missing_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.toml");
    let output = common::dipper()
        .args(["check", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("config file not found"), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
}

#[test]
fn cli_missing_table() {
    assert_check_error(
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
columns = ["ORDER_ID"]
"#,
        &["dips[0]", "table"],
    );
}

#[test]
fn cli_unknown_source() {
    assert_check_error(
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "nope"
table = "ORDERS"
columns = ["ORDER_ID"]
"#,
        &["dips[0].source", "nope"],
    );
}

#[test]
fn cli_duplicate_dip_name() {
    assert_check_error(
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]

[[dips]]
name = "Orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]
"#,
        &["dips[1].name", "duplicate"],
    );
}

#[test]
fn cli_invalid_identifier() {
    assert_check_error(
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS; DROP"
columns = ["ORDER_ID"]
"#,
        &["dips[0].table", "invalid identifier"],
    );
}

#[test]
fn cli_unknown_filter_op() {
    assert_check_error(
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]
filters = [{ column = "STATUS", op = "contains", value = "X" }]
"#,
        &["dips[0].filters[0].op", "contains"],
    );
}

#[test]
fn cli_invalid_memory_limit() {
    assert_check_error(
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[performance]
memory_limit = "2 gigs"
"#,
        &["performance.memory_limit"],
    );
}

#[test]
fn cli_unknown_key() {
    assert_check_error(
        r#"
[sources.sample]
kind = "sqlite"
path = "sample.db"

[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
colums = ["ORDER_ID"]
"#,
        &["dips[0]", "colums"],
    );
}

#[test]
fn cli_unset_password_env() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_config(
        dir.path(),
        r#"
[sources.company]
kind = "oracle"
connect = "127.0.0.1:1/NOPE"
user = "scott"
password = "${DIPPER_TEST_UNSET_VAR}"
"#,
    );
    let output = common::dipper()
        .env_remove("DIPPER_TEST_UNSET_VAR")
        .args(["check", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("sources.company.password"), "{stderr}");
    assert!(stderr.contains("DIPPER_TEST_UNSET_VAR"), "{stderr}");
}

#[test]
fn cli_literal_password_is_rejected_without_echo() {
    let (code, stdout, stderr) = run_check(
        r#"
[sources.company]
kind = "oracle"
connect = "127.0.0.1:1/NOPE"
user = "scott"
password = "PR2-canary-literal"
"#,
    );
    assert_eq!(code, 1);
    assert!(stderr.contains("sources.company.password"), "{stderr}");
    assert!(stderr.contains("${"), "{stderr}");
    assert!(!stderr.contains("PR2-canary-literal"), "{stderr}");
    assert!(!stdout.contains("PR2-canary-literal"), "{stdout}");
}

#[test]
fn cli_password_syntax_error_hides_value() {
    let (code, stdout, stderr) = run_check("password = \"PR2-canary-syntax\n");
    assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(!stderr.contains("PR2-canary-syntax"), "{stderr}");
    assert!(!stdout.contains("PR2-canary-syntax"), "{stdout}");
}
