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
    let output = Command::cargo_bin("dipper")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines = stdout.lines();
    let commands_at = lines.position(|line| line == "Commands:");
    assert!(
        commands_at.is_some(),
        "Commands: section not found in:\n{stdout}"
    );
    let mut commands = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        let trimmed = line.trim_start();
        let word = trimmed.split(' ').next().unwrap_or("");
        if !word.is_empty() {
            commands.push(word);
        }
    }
    assert_eq!(
        commands,
        ["check", "dip", "tables", "show", "stats", "export", "help"]
    );
}

#[test]
fn subcommands_are_not_implemented() {
    for name in ["dip", "tables", "show", "stats", "export"] {
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

#[test]
fn check_without_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("dipper")
        .unwrap()
        .current_dir(dir.path())
        .arg("check")
        .assert()
        .code(1);
    let stderr = String::from_utf8_lossy(&output.get_output().stderr);
    assert!(stderr.contains("config file not found"), "stderr: {stderr}");
    assert!(stderr.contains("dipper.toml"), "stderr: {stderr}");
}
