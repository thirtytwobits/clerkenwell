use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clerkenwell_codegen::{generate, Config, Mode};

/// Validates a projection definition and generates its Rust, TypeScript,
/// fixture and coverage artefacts.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The generator configuration. Paths inside it resolve relative to it.
    #[arg(long)]
    config: PathBuf,
    /// Write nothing; exit non-zero listing every stale or missing artefact.
    #[arg(long)]
    check: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mode = if cli.check { Mode::Check } else { Mode::Write };
    match Config::load(&cli.config).and_then(|config| generate(&config, mode)) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
