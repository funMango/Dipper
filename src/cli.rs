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

/// Subcommands. Arguments are added in later PRs.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check that configured sources are reachable.
    Check,
    /// Copy selected data from sources into the local store.
    Dip {
        /// Dip names to run (default: all, in config order).
        names: Vec<String>,
        /// Stop at the first failed dip.
        #[arg(long)]
        fail_fast: bool,
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
        Command::Dip { names, fail_fast } => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            crate::commands::dip::run(
                &cli.config,
                &names,
                fail_fast,
                cli.strict,
                cli.log_sql,
                &mut handle,
            )
        }
        Command::Tables => Err(DipperError::NotImplemented("tables")),
        Command::Show => Err(DipperError::NotImplemented("show")),
        Command::Stats => Err(DipperError::NotImplemented("stats")),
        Command::Export => Err(DipperError::NotImplemented("export")),
    }
}
