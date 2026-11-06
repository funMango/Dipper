use std::path::PathBuf;

use thiserror::Error;

use crate::sql::guard::GuardError;

/// Errors returned by the Dipper library.
#[derive(Debug, Error)]
pub enum DipperError {
    /// The command exists but is not implemented yet.
    #[error("{0}: not implemented yet")]
    NotImplemented(&'static str),

    /// The config file does not exist.
    #[error("config file not found: {0}")]
    ConfigNotFound(PathBuf),

    /// The config file exists but could not be read.
    #[error("cannot read config file {path}: {source}")]
    ConfigRead {
        path: PathBuf,
        source: std::io::Error,
    },

    /// The config file was read but is not valid.
    #[error("invalid config {file}:\n{}", format_issues(.issues))]
    Config {
        file: PathBuf,
        issues: Vec<ConfigIssue>,
    },

    /// A source could not be opened or checked.
    #[error("{source_name}: {message}")]
    Source {
        source_name: String,
        message: String,
    },

    /// One or more sources failed `dipper check`.
    #[error("check failed: {failed} of {total} sources failed")]
    CheckFailed { failed: usize, total: usize },

    /// SQL was rejected by the AST guard.
    #[error("{0}")]
    Guard(#[from] GuardError),
}

/// One problem found while loading a config file.
///
/// `line_col` is 1-based when the TOML span is known. Messages never include
/// the original source line, because that line may contain a password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub path: String,
    pub line_col: Option<(usize, usize)>,
    pub message: String,
}

impl DipperError {
    /// 종료코드 규약: 0 성공, 1 설정·실행 오류, 2 사용법 오류·미구현.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotImplemented(_) => 2,
            Self::ConfigNotFound(_)
            | Self::ConfigRead { .. }
            | Self::Config { .. }
            | Self::Source { .. }
            | Self::CheckFailed { .. }
            | Self::Guard(_) => 1,
        }
    }
}

fn format_issues(issues: &[ConfigIssue]) -> String {
    issues
        .iter()
        .map(|issue| match (issue.line_col, issue.path.is_empty()) {
            (Some((line, col)), false) => {
                format!("  {line}:{col}: {}: {}", issue.path, issue.message)
            }
            (Some((line, col)), true) => format!("  {line}:{col}: {}", issue.message),
            (None, false) => format!("  {}: {}", issue.path, issue.message),
            (None, true) => format!("  {}", issue.message),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
