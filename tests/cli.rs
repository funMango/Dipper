use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn version_prints_package_version() {
    Command::cargo_bin("dipper")
        .unwrap()
        .arg("--version")
        .assert()
        .code(0)
        .stdout(format!("dipper {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_lists_subcommands() {
    Command::cargo_bin("dipper")
        .unwrap()
        .arg("--help")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("check"))
        .stdout(predicate::str::contains("dip"))
        .stdout(predicate::str::contains("tables"))
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("stats"))
        .stdout(predicate::str::contains("export"));
}

#[test]
fn subcommands_are_not_implemented() {
    for name in ["check", "dip", "tables", "show", "stats", "export"] {
        Command::cargo_bin("dipper")
            .unwrap()
            .arg(name)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("not implemented yet"));
    }
}

#[test]
fn no_args_shows_usage() {
    let assertion = Command::cargo_bin("dipper").unwrap().assert().code(2);
    let output = assertion.get_output();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("Usage"),
        "expected Usage in output, got: {combined}"
    );
}
