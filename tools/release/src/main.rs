use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use clerkenwell_release::{
    agreed, bump, check_release, index_documentation, release_notes, workspace, Version, CONTRACTS,
};

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
    /// patch release changes a public-API record, a wire-protocol record, the
    /// definition meta-schema or the stored envelope version.
    Breaking {
        /// The previous release's Git reference, such as its tag.
        #[arg(long)]
        since: String,
    },
    /// Prints VERSION's section of CHANGELOG.md, the notes of its release.
    Notes { version: Version },
    /// Writes the documentation site into DIRECTORY: README.md as its front
    /// page, linking each workspace library's documentation, and each
    /// Markdown document README.md reaches by its links. Prints each page
    /// written; exits non-zero when a library is undocumented, the
    /// documentation names Loro, or a document links a missing file.
    Docs {
        /// The directory `cargo doc --workspace --no-deps` wrote.
        directory: PathBuf,
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
        Commands::Notes { version } => release_notes(&root, version).map(|notes| print!("{notes}")),
        Commands::Docs { directory } => index_documentation(&root, &directory).map(|site| {
            for page in site.pages {
                println!("{page}");
            }
        }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
