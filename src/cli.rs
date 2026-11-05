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
    #[command(subcommand)]
    command: Command,
}

/// Subcommands. Arguments are added in later PRs.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check that configured sources are reachable.
    Check,
    /// Copy selected data from sources into the local store.
    Dip,
    /// List tables in the local store.
    Tables,
    /// Show rows from a local table.
    Show,
    /// Show basic statistics for a local table.
    Stats,
    /// Export a local table to a file.
    Export,
}

/// Run a parsed command. Every command is a placeholder until a later PR.
pub fn run(cli: Cli) -> Result<(), DipperError> {
    match cli.command {
        Command::Check => Err(DipperError::NotImplemented("check")),
        Command::Dip => Err(DipperError::NotImplemented("dip")),
        Command::Tables => Err(DipperError::NotImplemented("tables")),
        Command::Show => Err(DipperError::NotImplemented("show")),
        Command::Stats => Err(DipperError::NotImplemented("stats")),
        Command::Export => Err(DipperError::NotImplemented("export")),
    }
}
