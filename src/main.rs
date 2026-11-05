use clap::Parser;
use dipper::cli::{run, Cli};

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("dipper: {err}");
        std::process::exit(err.exit_code());
    }
}
