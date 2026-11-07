//! Stream source chunks into the local store, one dip at a time.

pub mod progress;

use std::io::Write;
use std::time::{Duration, Instant, SystemTime};

use crate::config::{Config, Dip, Paging as ConfigPaging, SourceConfig};
use crate::error::DipperError;
use crate::source::{Finding, Level, OpenOptions, Opened, Paging, SelectSpec, Source, SqlLogSink};
use crate::store::{RunRecord, Store};

/// How `run` treats failures, warnings, and statement logging.
pub struct RunOptions {
    pub fail_fast: bool,
    pub strict: bool,
    pub log: SqlLogSink,
}

/// Opens a source. The default calls [`crate::source::open`]; tests can substitute a fake.
pub trait SourceOpener {
    fn open(
        &self,
        name: &str,
        cfg: &SourceConfig,
        opts: &OpenOptions,
    ) -> Result<Opened, DipperError>;
}

/// Delegates to the read-only source API.
pub struct DefaultOpener;

impl SourceOpener for DefaultOpener {
    fn open(
        &self,
        name: &str,
        cfg: &SourceConfig,
        opts: &OpenOptions,
    ) -> Result<Opened, DipperError> {
        crate::source::open(name, cfg, opts)
    }
}

/// Result of one dip that was started.
pub struct DipOutcome {
    pub dip: String,
    pub source: String,
    pub rows: u64,
    pub elapsed: Duration,
    pub warnings: u64,
    pub paging: &'static str,
    pub result: Result<(), String>,
}

/// Outcomes in run order, plus dips skipped after `--fail-fast`.
pub struct RunReport {
    pub outcomes: Vec<DipOutcome>,
    pub not_run: Vec<String>,
}

impl RunReport {
    pub fn failed(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.result.is_err())
            .count()
    }

    pub fn total(&self) -> usize {
        self.outcomes.len() + self.not_run.len()
    }
}

/// Empty `names` selects every dip. Matching is case-insensitive and duplicates count once.
///
/// The result stays in config order. The first unknown name is [`DipperError::UnknownDip`].
pub fn select_dips<'c>(config: &'c Config, names: &[String]) -> Result<Vec<&'c Dip>, DipperError> {
    if names.is_empty() {
        return Ok(config.dips.iter().collect());
    }
    for name in names {
        let found = config
            .dips
            .iter()
            .any(|dip| dip.name.eq_ignore_ascii_case(name));
        if !found {
            return Err(DipperError::UnknownDip(name.clone()));
        }
    }
    Ok(config
        .dips
        .iter()
        .filter(|dip| {
            names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&dip.name))
        })
        .collect())
}

/// Same field mapping as the private source helper. `batch_rows` comes from performance settings.
pub fn spec_for(config: &Config, dip: &Dip) -> SelectSpec {
    SelectSpec {
        table: dip.table.clone(),
        columns: dip.columns.clone(),
        filters: dip.filters.clone(),
        limit: dip.limit,
        batch_rows: config.performance.batch_rows,
        key: dip.key.clone(),
        paging: match dip.paging {
            ConfigPaging::Keyset => Paging::Keyset,
            ConfigPaging::Snapshot => Paging::Snapshot,
        },
    }
}

/// Run the selected dips in order. Each source is opened and closed before the next dip.
pub fn run(
    config: &Config,
    dips: &[&Dip],
    store: &Store,
    opts: &RunOptions,
    opener: &dyn SourceOpener,
    progress: &mut dyn progress::Progress,
    events: &mut dyn Write,
) -> RunReport {
    let mut outcomes = Vec::new();
    let mut not_run = Vec::new();
    for (index, dip) in dips.iter().enumerate() {
        let outcome = run_one(config, dip, store, opts, opener, progress, events);
        let failed = outcome.result.is_err();
        outcomes.push(outcome);
        if failed && opts.fail_fast {
            not_run.extend(dips[index + 1..].iter().map(|dip| dip.name.clone()));
            break;
        }
    }
    RunReport { outcomes, not_run }
}

struct Attempt {
    rows: u64,
    warnings: u64,
    result: Result<(), String>,
}

fn run_one(
    config: &Config,
    dip: &Dip,
    store: &Store,
    opts: &RunOptions,
    opener: &dyn SourceOpener,
    progress: &mut dyn progress::Progress,
    events: &mut dyn Write,
) -> DipOutcome {
    let started_at = SystemTime::now();
    let timer = Instant::now();
    let paging = paging_name(dip.paging);
    let attempt = attempt_dip(config, dip, store, opts, opener, progress, events);
    let finished_at = SystemTime::now();
    let mut result = attempt.result;
    let record_error = result.as_ref().err().cloned();
    if let Err(err) = store.record_run(&RunRecord {
        dip: dip.name.clone(),
        source: dip.source.clone(),
        started_at,
        finished_at,
        rows: attempt.rows,
        ok: result.is_ok(),
        error: record_error,
        paging,
        warnings: attempt.warnings,
    }) {
        let message = err.to_string();
        result = Err(match result {
            Ok(()) => message,
            Err(previous) => format!("{previous}; {message}"),
        });
    }
    DipOutcome {
        dip: dip.name.clone(),
        source: dip.source.clone(),
        rows: attempt.rows,
        elapsed: timer.elapsed(),
        warnings: attempt.warnings,
        paging,
        result,
    }
}

fn attempt_dip(
    config: &Config,
    dip: &Dip,
    store: &Store,
    opts: &RunOptions,
    opener: &dyn SourceOpener,
    progress: &mut dyn progress::Progress,
    events: &mut dyn Write,
) -> Attempt {
    let spec = spec_for(config, dip);
    let Some(source_cfg) = config.sources.get(&dip.source) else {
        return fail(0, 0, format!("unknown source: {}", dip.source));
    };
    let open_opts = OpenOptions {
        strict: opts.strict,
        log: opts.log.clone(),
        action: format!("dip:{}", dip.name),
    };
    let opened = match opener.open(&dip.source, source_cfg, &open_opts) {
        Ok(opened) => opened,
        Err(err) => return fail(0, 0, err.to_string()),
    };

    let mut warnings = warn_count(&opened.findings);
    if let Err(message) = write_findings(events, &dip.name, &opened.findings) {
        drop(opened);
        return fail(0, warnings, message);
    }
    let report = match opened.source.preflight(&dip.name, &spec) {
        Ok(report) => report,
        Err(err) => {
            drop(opened);
            return fail(0, warnings, err.to_string());
        }
    };
    warnings += warn_count(&report.findings);
    if let Err(message) = write_findings(events, &dip.name, &report.findings) {
        drop(opened);
        return fail(0, warnings, message);
    }
    let findings: Vec<Finding> = opened
        .findings
        .iter()
        .cloned()
        .chain(report.findings.iter().cloned())
        .collect();
    if let Some(reason) = block_reason(&findings, opts.strict) {
        drop(opened);
        return fail(0, warnings, reason);
    }

    let total = if dip.count_first {
        match opened.source.count(&spec) {
            Ok(count) => Some(count),
            Err(err) => {
                drop(opened);
                return fail(0, warnings, err.to_string());
            }
        }
    } else {
        None
    };

    let mut load = match store.begin_load(&dip.name) {
        Ok(load) => load,
        Err(err) => {
            drop(opened);
            return fail(0, warnings, err.to_string());
        }
    };
    progress.start(&dip.name, total);
    let loaded = read_into(opened.source.as_ref(), &spec, &mut load, progress);
    drop(opened);
    progress.finish();
    match loaded {
        Ok(()) => {
            let rows = load.rows();
            match load.finish(&dip.columns) {
                Ok(finished) => Attempt {
                    rows: finished,
                    warnings,
                    result: Ok(()),
                },
                Err(err) => fail(rows, warnings, err.to_string()),
            }
        }
        Err(err) => {
            let rows = load.rows();
            drop(load);
            fail(rows, warnings, err.to_string())
        }
    }
}

fn read_into(
    source: &dyn Source,
    spec: &SelectSpec,
    load: &mut crate::store::TableLoad<'_>,
    progress: &mut dyn progress::Progress,
) -> Result<(), DipperError> {
    let chunks = source.select(spec)?;
    for chunk in chunks {
        let chunk = chunk?;
        load.append(&chunk)?;
        progress.advance(load.rows());
        drop(chunk);
    }
    Ok(())
}

fn fail(rows: u64, warnings: u64, message: String) -> Attempt {
    Attempt {
        rows,
        warnings,
        result: Err(message),
    }
}

fn warn_count(findings: &[Finding]) -> u64 {
    findings
        .iter()
        .filter(|finding| finding.level == Level::Warn)
        .count() as u64
}

/// Refuse always stops. A warning stops only when strict mode is on.
fn block_reason(findings: &[Finding], strict: bool) -> Option<String> {
    if let Some(finding) = findings
        .iter()
        .find(|finding| finding.level == Level::Refuse)
    {
        return Some(format!("refused: {}", finding.message));
    }
    if strict {
        let warnings = warn_count(findings);
        if warnings > 0 {
            return Some(format!("strict: {warnings} warning(s)"));
        }
    }
    None
}

fn write_findings(events: &mut dyn Write, dip: &str, findings: &[Finding]) -> Result<(), String> {
    for finding in findings {
        let label = match finding.level {
            Level::Note => "note",
            Level::Warn => "warning",
            Level::Refuse => "refused",
        };
        let name = finding
            .dip
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(dip);
        writeln!(events, "{label}: {name}: {}", finding.message).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn paging_name(paging: ConfigPaging) -> &'static str {
    match paging {
        ConfigPaging::Keyset => "keyset",
        ConfigPaging::Snapshot => "snapshot",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Filter, Performance, Secret, SourceConfig};
    use crate::source::{Chunk, ChunkIter, KeySource, PreflightReport, ResolvedKey, Value};
    use crate::store::Store;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn dip(name: &str, source: &str) -> Dip {
        Dip {
            name: name.to_string(),
            source: source.to_string(),
            table: "ORDERS".to_string(),
            columns: vec!["ORDER_ID".to_string()],
            filters: Vec::<Filter>::new(),
            limit: None,
            count_first: false,
            key: None,
            paging: ConfigPaging::Keyset,
        }
    }

    fn config_with(dips: Vec<Dip>) -> Config {
        let mut sources = BTreeMap::new();
        sources.insert(
            "sample".to_string(),
            SourceConfig::Sqlite {
                path: PathBuf::from("missing.db"),
                max_session_minutes: 480,
            },
        );
        sources.insert(
            "company".to_string(),
            SourceConfig::Oracle {
                connect: "127.0.0.1:1/NOPE".to_string(),
                user: "reader".to_string(),
                password: Secret::new("hidden".to_string()),
                call_timeout_secs: 300,
                max_session_minutes: 480,
            },
        );
        Config {
            file: PathBuf::from("dipper.toml"),
            strict_readonly: false,
            store: PathBuf::from("store.duckdb"),
            performance: Performance {
                batch_rows: 10,
                memory_limit: "64MiB".to_string(),
                temp_directory: PathBuf::from("tmp"),
                progress_every_rows: 1000,
            },
            sources,
            dips,
        }
    }

    #[test]
    fn select_dips_follows_config_order_and_rejects_unknown_names() {
        let config = config_with(vec![dip("orders", "sample"), dip("customers", "sample")]);
        let all = select_dips(&config, &[]).unwrap();
        assert_eq!(
            all.iter().map(|dip| dip.name.as_str()).collect::<Vec<_>>(),
            ["orders", "customers"]
        );
        let selected = select_dips(
            &config,
            &[
                "CUSTOMERS".to_string(),
                "customers".to_string(),
                "orders".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|dip| dip.name.as_str())
                .collect::<Vec<_>>(),
            ["orders", "customers"]
        );
        let err = select_dips(&config, &["orders".to_string(), "nope".to_string()]).unwrap_err();
        assert!(matches!(err, DipperError::UnknownDip(name) if name == "nope"));
    }

    #[derive(Default)]
    struct Calls {
        preflight: usize,
        count: usize,
        select: usize,
    }

    struct ProbeSource {
        calls: Arc<Mutex<Calls>>,
        preflight_findings: Vec<Finding>,
        rows: Vec<Vec<Value>>,
        fail_select: bool,
    }

    impl Source for ProbeSource {
        fn name(&self) -> &str {
            "sample"
        }
        fn kind(&self) -> &'static str {
            "sqlite"
        }
        fn test_connection(&self) -> Result<(), DipperError> {
            Ok(())
        }
        fn preflight(&self, dip: &str, spec: &SelectSpec) -> Result<PreflightReport, DipperError> {
            self.calls.lock().unwrap().preflight += 1;
            let _ = (dip, spec);
            Ok(PreflightReport {
                findings: self.preflight_findings.clone(),
                key: Some(ResolvedKey {
                    column: "ORDER_ID".to_string(),
                    source: KeySource::Declared,
                }),
                target: spec.table.clone(),
            })
        }
        fn count(&self, _spec: &SelectSpec) -> Result<u64, DipperError> {
            self.calls.lock().unwrap().count += 1;
            Ok(self.rows.len() as u64)
        }
        fn select<'a>(&'a self, _spec: &SelectSpec) -> Result<ChunkIter<'a>, DipperError> {
            self.calls.lock().unwrap().select += 1;
            if self.fail_select {
                return Err(DipperError::Source {
                    source_name: "sample".to_string(),
                    message: "select should not run".to_string(),
                });
            }
            let chunk = Chunk {
                columns: vec!["ORDER_ID".to_string()],
                rows: self.rows.clone(),
            };
            Ok(Box::new(std::iter::once(Ok(chunk))))
        }
    }

    struct ProbeOpener {
        calls: Arc<Mutex<Calls>>,
        fail_open: Option<String>,
        findings: Vec<Finding>,
        preflight: Vec<Finding>,
        rows: Vec<Vec<Value>>,
        fail_select: bool,
    }

    impl SourceOpener for ProbeOpener {
        fn open(
            &self,
            name: &str,
            _cfg: &SourceConfig,
            _opts: &OpenOptions,
        ) -> Result<Opened, DipperError> {
            if let Some(message) = &self.fail_open {
                return Err(DipperError::Source {
                    source_name: name.to_string(),
                    message: message.clone(),
                });
            }
            Ok(Opened {
                source: Box::new(ProbeSource {
                    calls: Arc::clone(&self.calls),
                    preflight_findings: self.preflight.clone(),
                    rows: self.rows.clone(),
                    fail_select: self.fail_select,
                }),
                findings: self.findings.clone(),
            })
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let perf = Performance {
            batch_rows: 10,
            memory_limit: "64MiB".to_string(),
            temp_directory: dir.path().join("tmp"),
            progress_every_rows: 1000,
        };
        let store = Store::open(&dir.path().join("store.duckdb"), &perf).unwrap();
        (dir, store)
    }

    fn finding(level: Level, message: &str) -> Finding {
        Finding {
            level,
            code: "test",
            dip: Some("orders".to_string()),
            message: message.to_string(),
        }
    }

    fn run_probe(
        dips: Vec<Dip>,
        opener: &ProbeOpener,
        strict: bool,
        fail_fast: bool,
    ) -> (RunReport, Store) {
        let config = config_with(dips);
        let (_dir, store) = store();
        let opts = RunOptions {
            fail_fast,
            strict,
            log: SqlLogSink::Off,
        };
        let mut progress = progress::LogProgress::new(Vec::<u8>::new(), 1000);
        let mut events = Vec::new();
        let selected: Vec<&Dip> = config.dips.iter().collect();
        let report = run(
            &config,
            &selected,
            &store,
            &opts,
            opener,
            &mut progress,
            &mut events,
        );
        (report, store)
    }

    #[test]
    fn open_failure_does_not_read_or_create_a_table() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let opener = ProbeOpener {
            calls: Arc::clone(&calls),
            fail_open: Some("ORA-28193 session is read-only".to_string()),
            findings: Vec::new(),
            preflight: Vec::new(),
            rows: Vec::new(),
            fail_select: true,
        };
        let (report, store) = run_probe(vec![dip("orders", "sample")], &opener, false, false);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.preflight, 0);
        assert_eq!(calls.count, 0);
        assert_eq!(calls.select, 0);
        assert!(report.outcomes[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("ORA-28193"));
        assert!(!store
            .testing_table_names()
            .unwrap()
            .iter()
            .any(|name| name == "orders"));
        let runs = store.testing_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].ok);
        assert!(runs[0].error.as_deref().unwrap().contains("ORA-28193"));
    }

    #[test]
    fn refuse_and_strict_warnings_skip_select_but_a_warning_loads() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let opener = ProbeOpener {
            calls: Arc::clone(&calls),
            fail_open: None,
            findings: Vec::new(),
            preflight: vec![finding(Level::Refuse, "object not found: NOPE")],
            rows: vec![vec![Value::Int(1)]],
            fail_select: true,
        };
        let (report, store) = run_probe(vec![dip("orders", "sample")], &opener, false, false);
        assert_eq!(calls.lock().unwrap().select, 0);
        assert!(report.outcomes[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("refused: object not found: NOPE"));
        assert!(!store
            .testing_table_names()
            .unwrap()
            .iter()
            .any(|name| name == "orders"));

        let calls = Arc::new(Mutex::new(Calls::default()));
        let opener = ProbeOpener {
            calls: Arc::clone(&calls),
            fail_open: None,
            findings: Vec::new(),
            preflight: vec![finding(Level::Warn, "target is a view (V_ORDERS)")],
            rows: vec![vec![Value::Int(1)]],
            fail_select: true,
        };
        let (report, _) = run_probe(vec![dip("orders", "sample")], &opener, true, false);
        assert_eq!(calls.lock().unwrap().select, 0);
        assert!(report.outcomes[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("strict: 1 warning(s)"));

        let calls = Arc::new(Mutex::new(Calls::default()));
        let opener = ProbeOpener {
            calls: Arc::clone(&calls),
            fail_open: None,
            findings: Vec::new(),
            preflight: vec![finding(Level::Warn, "target is a view (V_ORDERS)")],
            rows: vec![vec![Value::Int(1)]],
            fail_select: false,
        };
        let (report, store) = run_probe(vec![dip("orders", "sample")], &opener, false, false);
        assert_eq!(calls.lock().unwrap().select, 1);
        assert!(report.outcomes[0].result.is_ok());
        assert_eq!(store.testing_count("orders").unwrap(), 1);
    }

    #[test]
    fn count_first_counts_once_and_fail_fast_skips_the_rest() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let opener = ProbeOpener {
            calls: Arc::clone(&calls),
            fail_open: None,
            findings: Vec::new(),
            preflight: Vec::new(),
            rows: vec![vec![Value::Int(1)], vec![Value::Int(2)]],
            fail_select: false,
        };
        let mut counted = dip("orders", "sample");
        counted.count_first = true;
        let (report, _) = run_probe(vec![counted], &opener, false, false);
        assert_eq!(calls.lock().unwrap().count, 1);
        assert_eq!(calls.lock().unwrap().select, 1);
        assert!(report.outcomes[0].result.is_ok());

        let calls = Arc::new(Mutex::new(Calls::default()));
        let opener = ProbeOpener {
            calls,
            fail_open: None,
            findings: Vec::new(),
            preflight: vec![finding(Level::Refuse, "object not found: NOPE")],
            rows: Vec::new(),
            fail_select: true,
        };
        let (report, store) = run_probe(
            vec![dip("orders", "sample"), dip("items", "sample")],
            &opener,
            false,
            true,
        );
        assert_eq!(report.not_run, ["items".to_string()]);
        assert_eq!(report.failed(), 1);
        assert_eq!(report.total(), 2);
        assert_eq!(store.testing_runs().unwrap().len(), 1);
    }
}
