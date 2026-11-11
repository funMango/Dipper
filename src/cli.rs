use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::error::DipperError;

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
    },
    /// Write `ontology/md` from extracted JSON plus any overlay.
    Render,
    /// Reject an overlay or extracted document that does not match the schema.
    Validate,
}

/// Subcommands. Arguments are added in later PRs.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check that configured sources are reachable.
    Check,
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
        Command::Check => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            crate::commands::check::run(&cli.config, cli.strict, cli.log_sql, &mut handle)
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
                &mut handle,
            )
        }
        Command::Ontology { action } => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            match action {
                OntologyCommand::Pull { source, table } => crate::commands::ontology::pull(
                    &cli.config,
                    &source,
                    table.as_deref(),
                    cli.log_sql,
                    cli.strict,
                    &mut handle,
                ),
                OntologyCommand::Render => {
                    crate::commands::ontology::render_docs(std::path::Path::new("."), &mut handle)
                }
                OntologyCommand::Validate => {
                    crate::commands::ontology::validate_docs(std::path::Path::new("."), &mut handle)
                }
            }
        }
        Command::Tables => Err(DipperError::NotImplemented("tables")),
        Command::Show => Err(DipperError::NotImplemented("show")),
        Command::Stats => Err(DipperError::NotImplemented("stats")),
        Command::Export => Err(DipperError::NotImplemented("export")),
    }
}
