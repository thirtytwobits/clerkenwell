use std::process::ExitCode;

use clap::Parser;
use clerkenwell_public_api::{record, workspace, COMMAND, RECORDED, RECORD_FILE};

/// Writes the public API record of each recorded Clerkenwell crate.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Write nothing; exit non-zero listing every record that differs.
    #[arg(long)]
    check: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut stale = Vec::new();
    for name in RECORDED {
        let dir = workspace().join("crates").join(name);
        let text = match record(&dir) {
            Ok(record) => record.text(),
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };
        let path = dir.join(RECORD_FILE);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            continue;
        }
        if cli.check {
            stale.push(path);
        } else if let Err(error) = std::fs::write(&path, text) {
            eprintln!("cannot write {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    }
    if stale.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!("Public API records are stale. Run `{COMMAND}` to update:");
    for path in stale {
        eprintln!("{}", path.display());
    }
    ExitCode::FAILURE
}
