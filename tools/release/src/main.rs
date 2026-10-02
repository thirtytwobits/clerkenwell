use std::process::ExitCode;

use clap::{Parser, Subcommand};
use clerkenwell_release::{agreed, bump, check_release, workspace, Version, CONTRACTS};

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
    /// Compares this release with the previous one; exits non-zero when a
    /// patch release changes a public-API record, the definition meta-schema
    /// or the stored envelope version.
    Breaking {
        /// The previous release's Git reference, such as its tag.
        #[arg(long)]
        since: String,
    },
}

fn main() -> ExitCode {
    let root = workspace();
    let result = match Cli::parse().command {
        Commands::Bump { version } => bump(&root, version).map(|version| println!("{version}")),
        Commands::Check => agreed(&root).map(|version| println!("{version}")),
        Commands::Breaking { since } => {
            check_release(&root, &since, CONTRACTS).map(|comparison| println!("{comparison}"))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
