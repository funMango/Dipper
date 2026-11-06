#![allow(dead_code)] // preflight checks are filled in with `dipper check`

/// How serious a preflight or session finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Note,
    Warn,
    Refuse,
}

/// One check result attached to a source session or a dip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub code: &'static str,
    pub dip: Option<String>,
    pub message: String,
}
