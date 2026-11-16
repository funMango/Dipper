use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::error::DipperError;

/// stdout shape. The default `table` output is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Table => "table",
            Self::Json => "json",
        })
    }
}

/// Command-line interface for Dipper.
#[derive(Debug, Parser)]
#[command(
    name = "dipper",
    version,
    about = "Dipper picks only the data you need out of multiple large databases and moves it into a small store, so you can search and analyze it quickly on your own PC or in the cloud.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Configuration file.
    #[arg(short, long, global = true, default_value = "dipper.toml")]
    config: PathBuf,

    /// Print debug logs on stderr.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Stop when a preflight warning is found.
    #[arg(long, global = true)]
    strict: bool,

    /// Print each statement sent to a source on stderr.
    #[arg(long, global = true)]
    log_sql: bool,

    #[command(subcommand)]
    command: Command,
}

/// `dipper ontology` actions.
#[derive(Debug, Subcommand)]
pub enum OntologyCommand {
    /// Read dictionary metadata into `ontology/extracted`. Overlay files are not changed.
    Pull {
        /// Source name from the config file.
        source: String,
        /// `OWNER.TABLE`. SQLite may use the table name alone (`main` is the owner).
        #[arg(long)]
        table: Option<String>,
        /// stdout format. `table` matches the previous line-oriented output.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
    },
    /// Write `ontology/md` from extracted JSON plus any overlay.
    Render,
    /// Reject an overlay or extracted document that does not match the schema.
    Validate,
    /// List tables and views from the dictionary. Does not read table rows.
    List {
        /// Source name from the config file.
        source: String,
        /// Oracle owner. Required for Oracle. SQLite defaults to `main`.
        #[arg(long)]
        owner: Option<String>,
        /// `LIKE` pattern. Default `%`.
        #[arg(long, default_value = "%")]
        name_like: String,
        /// Maximum rows. Default 200, maximum 2000.
        #[arg(long)]
        limit: Option<u64>,
        /// stdout format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
    },
    /// Find tables that contain a column. Does not read table rows.
    Find {
        /// Source name from the config file.
        source: String,
        /// Column name. Matched exactly after ASCII uppercase normalization.
        #[arg(long)]
        column: String,
        /// Restrict the search to this owner.
        #[arg(long)]
        owner: Option<String>,
        /// Maximum rows. Default 200, maximum 2000.
        #[arg(long)]
        limit: Option<u64>,
        /// stdout format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
    },
    /// Count how values in one table overlap another. Counts only, no row values.
    CheckRelation {
        /// Source name from the config file.
        source: String,
        /// Source table, `OWNER.TABLE`. SQLite may omit the owner.
        #[arg(long)]
        from_table: String,
        /// Source column. Repeat for a composite key.
        #[arg(long, required = true)]
        from_column: Vec<String>,
        /// Target table, `OWNER.TABLE`. SQLite may omit the owner.
        #[arg(long)]
        to_table: String,
        /// Target column. Repeat once per `--from-column`.
        #[arg(long, required = true)]
        to_column: Vec<String>,
        /// stdout format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
    },
}

/// Subcommands. Arguments are added in later PRs.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check the environment, then that configured sources are reachable.
    Check {
        /// Print the environment section and do not open a source.
        #[arg(long)]
        env_only: bool,
        /// stdout format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
    },
    /// Copy selected data from sources into the local store.
    ///
    /// `--from` and `--to` load the missing part of a period. Ranges already
    /// in the slice ledger are kept. `--refresh` reloads the requested window.
    /// `--replace-table` replaces the whole local table.
    Dip {
        /// Dip names to run (default: all, in config order).
        names: Vec<String>,
        /// Stop at the first failed dip.
        #[arg(long)]
        fail_fast: bool,
        /// Period start (`YYMMDD`, `YYYY-MM-DD`, or `YYYY-MM-DDTHH:MM:SS`). Requires `--to`.
        #[arg(long)]
        from: Option<String>,
        /// Period end. A date includes that day. A timestamp is exclusive (`column < T`, that instant is not included). The same timestamp as `--from` loads only that instant (`column = T`).
        #[arg(long)]
        to: Option<String>,
        /// Time column used by `--from` and `--to`.
        #[arg(long)]
        time_column: Option<String>,
        /// Skip the proceed prompt. Does not skip the 31-day limit on a missing range, the row limit, or the missing-ledger check.
        #[arg(long)]
        yes: bool,
        /// Replace an existing local table with the requested window. Cannot be combined with `--refresh`.
        #[arg(long)]
        replace_table: bool,
        /// Reload the requested window and replace only that range in the local table.
        #[arg(long)]
        refresh: bool,
        /// stdout format. `table` is the summary table. `json` is one document.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
    },
    /// Read a source dictionary into ontology JSON, or check and render it.
    Ontology {
        #[command(subcommand)]
        action: OntologyCommand,
    },
    /// List tables in the local store.
    Tables,
    /// Show rows from a local table.
    Show,
    /// Show basic statistics for a local table.
    Stats,
    /// Export a local table to a file.
    Export,
}

/// Run a parsed command.
pub fn run(cli: Cli) -> Result<(), DipperError> {
    let level = if cli.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .try_init();

    match cli.command {
        Command::Check { env_only, format } => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            crate::commands::check::run(
                &cli.config,
                cli.strict,
                cli.log_sql,
                env_only,
                format,
                &mut handle,
            )
        }
        Command::Dip {
            names,
            fail_fast,
            from,
            to,
            time_column,
            yes,
            replace_table,
            refresh,
            format,
        } => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            crate::commands::dip::run(
                &cli.config,
                &names,
                fail_fast,
                cli.strict,
                cli.log_sql,
                crate::commands::dip::PeriodCli {
                    from,
                    to,
                    time_column,
                    yes,
                    replace_table,
                    refresh,
                },
                format,
                &mut handle,
            )
        }
        Command::Ontology { action } => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            match action {
                OntologyCommand::Pull {
                    source,
                    table,
                    format,
                } => crate::commands::ontology::pull(
                    &cli.config,
                    &source,
                    table.as_deref(),
                    cli.log_sql,
                    cli.strict,
                    format,
                    &mut handle,
                ),
                OntologyCommand::Render => {
                    crate::commands::ontology::render_docs(std::path::Path::new("."), &mut handle)
                }
                OntologyCommand::Validate => {
                    crate::commands::ontology::validate_docs(std::path::Path::new("."), &mut handle)
                }
                OntologyCommand::List {
                    source,
                    owner,
                    name_like,
                    limit,
                    format,
                } => crate::commands::ontology::list(
                    &cli.config,
                    &source,
                    owner.as_deref(),
                    &name_like,
                    limit,
                    cli.log_sql,
                    cli.strict,
                    format,
                    &mut handle,
                ),
                OntologyCommand::Find {
                    source,
                    column,
                    owner,
                    limit,
                    format,
                } => crate::commands::ontology::find(
                    &cli.config,
                    &source,
                    &column,
                    owner.as_deref(),
                    limit,
                    cli.log_sql,
                    cli.strict,
                    format,
                    &mut handle,
                ),
                OntologyCommand::CheckRelation {
                    source,
                    from_table,
                    from_column,
                    to_table,
                    to_column,
                    format,
                } => crate::commands::ontology::check_relation(
                    &cli.config,
                    &source,
                    &from_table,
                    &from_column,
                    &to_table,
                    &to_column,
                    cli.log_sql,
                    cli.strict,
                    format,
                    &mut handle,
                ),
            }
        }
        Command::Tables => Err(DipperError::NotImplemented("tables")),
        Command::Show => Err(DipperError::NotImplemented("show")),
        Command::Stats => Err(DipperError::NotImplemented("stats")),
        Command::Export => Err(DipperError::NotImplemented("export")),
    }
}
