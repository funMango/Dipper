use std::path::PathBuf;

use thiserror::Error;

use crate::sql::guard::GuardError;

/// Why a command failed. The code stays visible after the message is redacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    ConnectFailed,
    ClientMissing,
    AuthFailed,
    ObjectNotFound,
    PermissionDenied,
    Timeout,
    SessionBusy,
    Config,
    Usage,
    Other,
}

impl ErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::ConnectFailed => "connect_failed",
            Self::ClientMissing => "client_missing",
            Self::AuthFailed => "auth_failed",
            Self::ObjectNotFound => "object_not_found",
            Self::PermissionDenied => "permission_denied",
            Self::Timeout => "timeout",
            Self::SessionBusy => "session_busy",
            Self::Config => "config",
            Self::Usage => "usage",
            Self::Other => "other",
        }
    }

    /// Only connection, timeout, and a busy session are worth one later retry.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::ConnectFailed | Self::Timeout | Self::SessionBusy
        )
    }

    /// Classify a driver or catalog message. Specific codes win over generic text.
    pub fn from_text(text: &str) -> Self {
        let upper = text.to_ascii_uppercase();
        if upper.contains("DPI-1047") {
            return Self::ClientMissing;
        }
        if upper.contains("ORA-01017") {
            return Self::AuthFailed;
        }
        if upper.contains("ORA-12170")
            || upper.contains("ORA-12541")
            || upper.contains("ORA-12514")
            || upper.contains("ORA-12537")
            || upper.contains("DPI-1080")
        {
            return Self::ConnectFailed;
        }
        if upper.contains("ORA-00942") || upper.contains("ORA-04043") {
            return Self::ObjectNotFound;
        }
        if upper.contains("ORA-01031") {
            return Self::PermissionDenied;
        }
        if upper.contains("DPI-1067") || upper.contains("ORA-03136") {
            return Self::Timeout;
        }
        if upper.contains("OBJECT NOT FOUND") || upper.contains("NO SUCH TABLE") {
            return Self::ObjectNotFound;
        }
        if upper.contains("CANNOT OPEN") {
            return Self::ConnectFailed;
        }
        Self::Other
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

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
    #[error("{source_name}: [{kind}] {message}")]
    Source {
        source_name: String,
        kind: ErrorKind,
        message: String,
    },

    /// One or more sources failed `dipper check`.
    #[error("check failed: {failed} of {total} sources failed")]
    CheckFailed { failed: usize, total: usize },

    /// SQL was rejected by the AST guard.
    #[error("{0}")]
    Guard(#[from] GuardError),

    /// Another source session is already open in this process.
    #[error("{0}")]
    SourceBusy(&'static str),

    /// Preflight refused to read a dip.
    #[error("refused: {dip}: {message}")]
    Refused {
        source_name: String,
        dip: String,
        message: String,
    },

    /// The source session outlived its deadline.
    #[error("{source_name}: session deadline exceeded")]
    DeadlineExceeded { source_name: String },

    /// Strict mode stopped on preflight warnings.
    #[error("{source_name}: strict: {warnings} warning(s)")]
    StrictReadonly {
        source_name: String,
        warnings: usize,
    },

    /// The local DuckDB store could not be opened or updated.
    #[error("store: {message}")]
    Store { message: String },

    /// One or more dips failed.
    #[error("dip failed: {failed} of {total} dips failed")]
    DipFailed { failed: usize, total: usize },

    /// The user named a dip that is not in the config.
    #[error("unknown dip: {0}")]
    UnknownDip(String),
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
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::ConfigNotFound(_) | Self::ConfigRead { .. } | Self::Config { .. } => {
                ErrorKind::Config
            }
            Self::NotImplemented(_) | Self::UnknownDip(_) => ErrorKind::Usage,
            Self::SourceBusy(_) => ErrorKind::SessionBusy,
            Self::DeadlineExceeded { .. } => ErrorKind::Timeout,
            Self::Source { kind, .. } => *kind,
            Self::CheckFailed { .. }
            | Self::Guard(_)
            | Self::Refused { .. }
            | Self::StrictReadonly { .. }
            | Self::Store { .. }
            | Self::DipFailed { .. } => ErrorKind::Other,
        }
    }

    pub fn retryable(&self) -> bool {
        self.kind().retryable()
    }

    /// JSON `error.message`. This is the text after redaction, without the bracket code.
    pub fn error_message(&self) -> String {
        match self {
            Self::Source { message, .. } => message.clone(),
            other => other.to_string(),
        }
    }

    /// 종료코드 규약: 0 성공, 1 설정·실행 오류, 2 사용법 오류·미구현.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotImplemented(_) | Self::UnknownDip(_) => 2,
            Self::ConfigNotFound(_)
            | Self::ConfigRead { .. }
            | Self::Config { .. }
            | Self::Source { .. }
            | Self::CheckFailed { .. }
            | Self::Guard(_)
            | Self::SourceBusy(_)
            | Self::Refused { .. }
            | Self::DeadlineExceeded { .. }
            | Self::StrictReadonly { .. }
            | Self::Store { .. }
            | Self::DipFailed { .. } => 1,
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
