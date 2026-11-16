use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::config::{self, SourceConfig};
use crate::error::DipperError;
use crate::output::ErrorBody;
use crate::source;

#[derive(Clone, Serialize)]
struct EnvItem {
    name: &'static str,
    status: String,
    detail: String,
    fix: Option<String>,
}

pub fn run(
    config_path: &Path,
    strict: bool,
    log_sql: bool,
    env_only: bool,
    format: crate::cli::OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let raw = std::fs::read_to_string(config_path).unwrap_or_default();
    let env = environment(config_path, &raw);
    let failed = env.iter().any(|item| item.status == "fail");
    let warned = env.iter().any(|item| item.status == "warn");
    let blocks_all = env
        .iter()
        .any(|item| item.name == "config" && item.status == "fail");
    let blocks_oracle = env.iter().any(|item| {
        (item.name == "oracle_feature" || item.name == "instant_client") && item.status == "fail"
    });
    let mut load_error = None;
    let mut config = if env_only || !config_path.is_file() {
        None
    } else {
        match config::load(config_path) {
            Ok(config) => Some(config),
            Err(err) => {
                load_error = Some(err);
                None
            }
        }
    };
    let strict = strict || config.as_ref().is_some_and(|config| config.strict_readonly);
    let sources = if blocks_all || env_only || config.is_none() {
        Vec::new()
    } else {
        let mut loaded = config.take().expect("config loaded");
        if blocks_oracle {
            loaded
                .sources
                .retain(|_, cfg| !matches!(cfg, SourceConfig::Oracle { .. }));
        }
        let opts = source::OpenOptions {
            strict,
            log: if log_sql {
                source::SqlLogSink::Stderr
            } else {
                source::SqlLogSink::Off
            },
            action: "check".to_string(),
        };
        let results = source::check_all(&loaded, &opts);
        config = Some(loaded);
        results
    };
    let source_failed = sources
        .iter()
        .filter(|result| result.outcome.is_err())
        .count();
    let exit_failed = failed || source_failed > 0 || (strict && warned);
    if format == crate::cli::OutputFormat::Json {
        let secrets = config
            .as_ref()
            .map(crate::output::oracle_secret_values)
            .unwrap_or_default();
        let body = CheckDocument {
            schema_version: 1,
            command: "check",
            ok: !exit_failed,
            exit_code: if exit_failed { 1 } else { 0 },
            env: env.clone(),
            sources: sources
                .iter()
                .map(|result| source_row(result, &secrets))
                .collect(),
            error: if exit_failed {
                Some(ErrorBody {
                    code: if failed {
                        "config".to_string()
                    } else {
                        "other".to_string()
                    },
                    retryable: false,
                    message: if failed {
                        "environment check failed".to_string()
                    } else {
                        format!(
                            "check failed: {source_failed} of {} sources failed",
                            sources.len()
                        )
                    },
                })
            } else {
                None
            },
        };
        let text = serde_json::to_string(&body).map_err(write_json_err)?;
        write_line(out, &text)?;
    } else {
        for item in &env {
            let fix = item
                .fix
                .as_ref()
                .map(|fix| format!(" fix: {fix}"))
                .unwrap_or_default();
            write_line(
                out,
                &format!("env {}: {} — {}{fix}", item.name, item.status, item.detail),
            )?;
        }
        for result in &sources {
            match &result.outcome {
                Ok(()) => write_line(out, &format!("{} ({}): OK", result.name, result.kind))?,
                Err(reason) => {
                    write_line(
                        out,
                        &format!("{} ({}): FAIL - {reason}", result.name, result.kind),
                    )?;
                }
            }
            for finding in &result.findings {
                write_line(out, &finding_line(finding))?;
            }
        }
        if !env_only && !failed {
            let ok = sources.len() - source_failed;
            write_line(out, &format!("{ok} of {} sources OK", sources.len()))?;
        }
    }
    if !config_path.is_file() {
        return Err(DipperError::ConfigNotFound(config_path.to_path_buf()));
    }
    if let Some(err) = load_error {
        return Err(err);
    }
    if exit_failed {
        Err(DipperError::CheckFailed {
            failed: source_failed.max(usize::from(failed)),
            total: if sources.is_empty() { 1 } else { sources.len() },
        })
    } else {
        Ok(())
    }
}

fn environment(config_path: &Path, raw: &str) -> Vec<EnvItem> {
    vec![
        executable_item(),
        config_item(config_path, raw),
        oracle_feature_item(raw),
        instant_client_item(raw),
    ]
}

fn executable_item() -> EnvItem {
    let exe = std::env::current_exe().ok();
    let dir = exe
        .as_ref()
        .and_then(|path| path.parent())
        .map(Path::to_path_buf);
    let on_path = dir.as_ref().is_some_and(|dir| {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|entry| paths_equal(&entry, dir)))
    });
    if on_path {
        EnvItem {
            name: "executable",
            status: "ok".to_string(),
            detail: "dipper's folder is on PATH".to_string(),
            fix: None,
        }
    } else {
        let shown = dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| ".".to_string());
        EnvItem {
            name: "executable",
            status: "warn".to_string(),
            detail: format!("dipper's folder is not on PATH ({shown})"),
            fix: Some(format!(
                "Add the dipper folder to the user PATH. PowerShell: [Environment]::SetEnvironmentVariable(\"Path\", $env:Path + \";{shown}\", \"User\")"
            )),
        }
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

fn config_item(config_path: &Path, raw: &str) -> EnvItem {
    if !config_path.is_file() {
        return EnvItem {
            name: "config",
            status: "fail".to_string(),
            detail: "config file was not found".to_string(),
            fix: Some("Create dipper.toml or pass --config".to_string()),
        };
    }
    let mut missing = Vec::new();
    let mut seen = Vec::new();
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == b'$' && bytes[index + 1] == b'{' {
            if let Some(end) = raw[index + 2..].find('}') {
                let name = &raw[index + 2..index + 2 + end];
                if is_env_name(name) && !seen.iter().any(|seen: &String| seen == name) {
                    seen.push(name.to_string());
                    if std::env::var_os(name).is_none() {
                        missing.push(name.to_string());
                    }
                }
                index += 2 + end;
                continue;
            }
        }
        index += 1;
    }
    if missing.is_empty() {
        EnvItem {
            name: "config",
            status: "ok".to_string(),
            detail: "every ${NAME} reference is set".to_string(),
            fix: None,
        }
    } else {
        let names = missing.join(", ");
        EnvItem {
            name: "config",
            status: "fail".to_string(),
            detail: format!("unset: {names}"),
            fix: Some(format!("Set the user environment variable `{names}`")),
        }
    }
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {
            chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        }
        _ => false,
    }
}

fn oracle_feature_item(raw: &str) -> EnvItem {
    let wants_oracle = raw.contains("kind = \"oracle\"") || raw.contains("kind = 'oracle'");
    let built = cfg!(feature = "oracle");
    if wants_oracle && !built {
        EnvItem {
            name: "oracle_feature",
            status: "fail".to_string(),
            detail: "this dipper binary was built without the oracle feature".to_string(),
            fix: Some("Rebuild dipper with --features oracle".to_string()),
        }
    } else {
        EnvItem {
            name: "oracle_feature",
            status: "ok".to_string(),
            detail: if built {
                "oracle feature is built in".to_string()
            } else {
                "no Oracle source is configured".to_string()
            },
            fix: None,
        }
    }
}

fn instant_client_item(raw: &str) -> EnvItem {
    let wants_oracle = raw.contains("kind = \"oracle\"") || raw.contains("kind = 'oracle'");
    match source::probe_instant_client() {
        source::InstantClient::Loaded(version) => EnvItem {
            name: "instant_client",
            status: "ok".to_string(),
            detail: format!("Oracle client {version}"),
            fix: None,
        },
        source::InstantClient::Unavailable(code) => EnvItem {
            name: "instant_client",
            status: if wants_oracle { "fail" } else { "warn" }.to_string(),
            detail: format!("Oracle Instant Client could not be loaded ({code})"),
            fix: Some(
                "Install Oracle Instant Client Basic or Light, add its folder to PATH, and see docs/ORACLE_SETUP.md"
                    .to_string(),
            ),
        },
        source::InstantClient::NotBuilt => EnvItem {
            name: "instant_client",
            status: "skipped".to_string(),
            detail: "Instant Client is not loaded in a build without the oracle feature".to_string(),
            fix: None,
        },
    }
}

fn source_row(result: &source::CheckResult, secrets: &[crate::output::KnownSecret]) -> SourceRow {
    let (status, error_code, message) = match &result.outcome {
        Ok(()) => ("ok".to_string(), None, None),
        Err(reason) => {
            let hidden = crate::output::redact_known_values(reason, secrets);
            let code = crate::error::ErrorKind::from_text(&hidden)
                .code()
                .to_string();
            ("fail".to_string(), Some(code), Some(hidden))
        }
    };
    let _ = message;
    SourceRow {
        name: result.name.clone(),
        kind: result.kind,
        status,
        error_code,
        findings: result
            .findings
            .iter()
            .map(|finding| finding.message.clone())
            .collect(),
    }
}

#[derive(Serialize)]
struct SourceRow {
    name: String,
    kind: &'static str,
    status: String,
    error_code: Option<String>,
    findings: Vec<String>,
}

#[derive(Serialize)]
struct CheckDocument {
    schema_version: i32,
    command: &'static str,
    ok: bool,
    exit_code: i32,
    env: Vec<EnvItem>,
    sources: Vec<SourceRow>,
    error: Option<ErrorBody>,
}

fn write_json_err(err: serde_json::Error) -> DipperError {
    DipperError::Source {
        kind: crate::error::ErrorKind::Other,
        source_name: "stdout".to_string(),
        message: err.to_string(),
    }
}

fn finding_line(finding: &source::Finding) -> String {
    let label = match finding.level {
        source::Level::Note => "note",
        source::Level::Warn => "warning",
        source::Level::Refuse => "refused",
    };
    match finding.dip.as_deref() {
        Some(dip) if !dip.is_empty() => format!("  {label}: {dip}: {}", finding.message),
        _ => format!("  {label}: {}", finding.message),
    }
}

fn write_line(out: &mut dyn Write, line: &str) -> Result<(), DipperError> {
    writeln!(out, "{line}").map_err(|err| DipperError::Source {
        kind: crate::error::ErrorKind::Other,
        source_name: "stdout".to_string(),
        message: err.to_string(),
    })
}
