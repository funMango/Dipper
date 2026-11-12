//! One JSON document for `dipper dip --format json`.

use std::io::Write;

use serde::Serialize;

use crate::error::DipperError;

#[derive(Debug, Clone, Serialize)]
pub struct PeriodBody {
    pub time_column: String,
    pub from: String,
    pub to: String,
    pub exact: bool,
    pub start: String,
    pub end: String,
    pub mode: String,
}

#[derive(Debug, Serialize)]
pub struct IgnoredFilter {
    pub column: String,
    pub op: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Interval {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchBody {
    pub start: String,
    pub end: String,
    pub count: u64,
}

#[derive(Debug, Serialize)]
pub struct DipBody {
    pub name: String,
    pub source: String,
    pub status: String,
    pub refresh: bool,
    pub ignored_filters: Vec<IgnoredFilter>,
    pub held: Vec<Interval>,
    pub fetch: Vec<FetchBody>,
    pub rows_loaded: u64,
    pub seconds: Option<f64>,
    pub warnings: u64,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SummaryBody {
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub not_run: usize,
    pub nothing_to_fetch: usize,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct DipDocument {
    pub schema_version: i32,
    pub command: &'static str,
    pub ok: bool,
    pub exit_code: i32,
    pub period: Option<PeriodBody>,
    pub dips: Vec<DipBody>,
    pub summary: SummaryBody,
    pub error: Option<ErrorBody>,
}

pub fn summary_of(dips: &[DipBody]) -> SummaryBody {
    SummaryBody {
        total: dips.len(),
        ok: dips.iter().filter(|dip| dip.status == "ok").count(),
        failed: dips.iter().filter(|dip| dip.status == "failed").count(),
        not_run: dips.iter().filter(|dip| dip.status == "not_run").count(),
        nothing_to_fetch: dips
            .iter()
            .filter(|dip| dip.status == "nothing_to_fetch")
            .count(),
    }
}

/// Failure document: no dip rows, and the same exit code `main` will use.
pub fn json_error_document(err: &DipperError, period: Option<PeriodBody>) -> DipDocument {
    let exit_code = err.exit_code();
    DipDocument {
        schema_version: 1,
        command: "dip",
        ok: false,
        exit_code,
        period,
        dips: Vec::new(),
        summary: SummaryBody {
            total: 0,
            ok: 0,
            failed: 0,
            not_run: 0,
            nothing_to_fetch: 0,
        },
        error: Some(ErrorBody {
            message: err.to_string(),
        }),
    }
}

pub fn write_json(out: &mut dyn Write, doc: &DipDocument) -> Result<(), DipperError> {
    let text = serde_json::to_string(doc).map_err(|err| DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    })?;
    writeln!(out, "{text}").map_err(|err| DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    })
}
