use std::io::Write;
use std::path::Path;

use crate::config::{self, SourceConfig};
use crate::error::DipperError;
use crate::source;

pub fn run(config_path: &Path, out: &mut dyn Write) -> Result<(), DipperError> {
    let config = config::load(config_path)?;
    tracing::debug!(path = %config.file.display(), "loaded config");
    for (name, source) in &config.sources {
        match source {
            SourceConfig::Sqlite { path } => {
                tracing::debug!(
                    source = %name,
                    kind = "sqlite",
                    path = %path.display(),
                    "source"
                );
            }
            SourceConfig::Oracle { connect, user, .. } => {
                tracing::debug!(
                    source = %name,
                    kind = "oracle",
                    connect = %connect,
                    user = %user,
                    "source"
                );
            }
        }
    }

    let opts = source::OpenOptions {
        strict: false,
        log: source::SqlLogSink::Off,
        action: "check".to_string(),
    };
    let results = source::check_all(&config, &opts);
    let total = results.len();
    let mut failed = 0usize;
    for result in &results {
        match &result.outcome {
            Ok(()) => write_line(out, &format!("{} ({}): OK", result.name, result.kind))?,
            Err(reason) => {
                failed += 1;
                write_line(
                    out,
                    &format!("{} ({}): FAIL - {reason}", result.name, result.kind),
                )?;
            }
        }
    }
    let ok = total - failed;
    write_line(out, &format!("{ok} of {total} sources OK"))?;
    if failed == 0 {
        Ok(())
    } else {
        Err(DipperError::CheckFailed { failed, total })
    }
}

fn write_line(out: &mut dyn Write, line: &str) -> Result<(), DipperError> {
    writeln!(out, "{line}").map_err(|err| DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    })
}
