use std::process::ExitCode;

use clap::{Parser, Subcommand};
use clerkenwell_release::{agreed, bump, workspace, Version};

/// Sets and checks the one version every Clerkenwell crate and package is
/// released at.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Moves every manifest and lockfile to VERSION.
    Bump {
        /// MAJOR.MINOR.PATCH, no lower than the workspace's version.
        version: Version,
    },
    /// Prints the version every manifest and lockfile states; exits non-zero
    /// listing every place that disagrees.
    Check,
}

fn main() -> ExitCode {
    let root = workspace();
    let result = match Cli::parse().command {
        Commands::Bump { version } => bump(&root, version),
        Commands::Check => agreed(&root),
    };
    match result {
        Ok(version) => {
            println!("{version}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
