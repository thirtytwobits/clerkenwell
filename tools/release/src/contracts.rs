//! The contracts a patch release may not change.
//!
//! A change to a public-API record, the definition meta-schema or the stored
//! envelope version is a breaking change, which in 0.x takes a minor release.
//! [`check_release`] compares the contracts in a working tree with those at
//! the previous release's Git reference, and refuses a patch release that
//! changes any of them.

use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::{agreed, parse_toml, read, Error, Version};

/// What a patch release may not change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contract {
    /// Every tracked file with this name.
    Records(&'static str),
    /// One file.
    File(&'static str),
    /// The declaration of a `const` in a Rust source file.
    Constant {
        file: &'static str,
        name: &'static str,
    },
}

/// Clerkenwell's contracts.
pub const CONTRACTS: &[Contract] = &[
    Contract::Records("public-api.txt"),
    Contract::Records("public-api.json"),
    Contract::File("crates/clerkenwell-codegen/src/projection-definition.schema.json"),
    Contract::Constant {
        file: "crates/clerkenwell-store/src/lib.rs",
        name: "ENVELOPE_VERSION",
    },
];

/// How one version follows another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    Major,
    Minor,
    Patch,
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Release::Major => "major",
            Release::Minor => "minor",
            Release::Patch => "patch",
        })
    }
}

/// A release measured against the previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub previous: Version,
    pub current: Version,
    pub release: Release,
    /// Each contract that changed, by file, and by constant where the
    /// contract is one.
    pub changed: Vec<String>,
}

/// Compares the working tree at `root` with the Git reference `since`, the
/// previous release. Refuses a version no newer than the previous release's,
/// and a patch release that changes any of `contracts`.
pub fn check_release(
    root: &Path,
    since: &str,
    contracts: &[Contract],
) -> Result<Comparison, Error> {
    let git = Git::at(root, since)?;
    let previous = previous_version(&git)?;
    let current = agreed(root)?;
    let release = release(previous, current).ok_or(Error::NotNewer { previous, current })?;
    let mut changed = Vec::new();
    for contract in contracts {
        changed.extend(changes(root, &git, *contract)?);
    }
    let comparison = Comparison {
        previous,
        current,
        release,
        changed,
    };
    if release == Release::Patch && !comparison.changed.is_empty() {
        return Err(Error::Breaking {
            previous,
            current,
            changed: comparison.changed,
        });
    }
    Ok(comparison)
}

/// The places in the working tree at `root` that `contract` names. A
/// contract that names nothing is refused, since it would gate nothing.
pub fn resolve(root: &Path, contract: Contract) -> Result<Vec<String>, Error> {
    match contract {
        Contract::Records(name) => {
            let files = tracked(root, name)?;
            if files.is_empty() {
                return Err(Error::Contract(format!("no tracked file is named {name}")));
            }
            Ok(files)
        }
        Contract::File(file) => {
            read(&root.join(file))?;
            Ok(vec![file.to_owned()])
        }
        Contract::Constant { file, name } => {
            let text = read(&root.join(file))?;
            declaration(&text, name)
                .ok_or_else(|| Error::Contract(format!("{file} declares no const {name}")))?;
            Ok(vec![format!("{file} {name}")])
        }
    }
}

fn release(previous: Version, current: Version) -> Option<Release> {
    if current <= previous {
        None
    } else if current.major != previous.major {
        Some(Release::Major)
    } else if current.minor != previous.minor {
        Some(Release::Minor)
    } else {
        Some(Release::Patch)
    }
}

/// The places `contract` names that differ between the working tree and the
/// previous release.
fn changes(root: &Path, git: &Git, contract: Contract) -> Result<Vec<String>, Error> {
    let current = |file: &str| -> Result<Option<String>, Error> {
        let path = root.join(file);
        if path.exists() {
            read(&path).map(Some)
        } else {
            Ok(None)
        }
    };
    match contract {
        Contract::Records(name) => {
            let mut files = tracked(root, name)?;
            files.extend(git.files_named(name)?);
            files.sort();
            files.dedup();
            let mut changed = Vec::new();
            for file in files {
                if current(&file)? != git.show(&file)? {
                    changed.push(file);
                }
            }
            Ok(changed)
        }
        Contract::File(file) => Ok(if current(file)? != git.show(file)? {
            vec![file.to_owned()]
        } else {
            Vec::new()
        }),
        Contract::Constant { file, name } => {
            let now = current(file)?;
            let then = git.show(file)?;
            let now = now.as_deref().and_then(|text| declaration(text, name));
            let then = then.as_deref().and_then(|text| declaration(text, name));
            Ok(if now != then {
                vec![format!("{file} {name}")]
            } else {
                Vec::new()
            })
        }
    }
}

/// The declaration of `const NAME`, from `const` to its `;`, with its
/// whitespace collapsed.
fn declaration(text: &str, name: &str) -> Option<String> {
    let needle = format!("const {name}");
    let start = text.match_indices(&needle).find_map(|(index, _)| {
        let after = text[index + needle.len()..].chars().next();
        after
            .filter(|next| !next.is_alphanumeric() && *next != '_')
            .map(|_| index)
    })?;
    let end = start + text[start..].find(';')?;
    Some(
        text[start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The files in the working tree at `root` that Git tracks or would track,
/// named `name`.
fn tracked(root: &Path, name: &str) -> Result<Vec<String>, Error> {
    let listed = git(
        root,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    )?;
    Ok(named(&listed, name)
        .filter(|file| root.join(file).exists())
        .collect())
}

fn named<'a>(listing: &'a str, name: &'a str) -> impl Iterator<Item = String> + 'a {
    listing
        .lines()
        .filter(move |file| Path::new(file).file_name().is_some_and(|file| file == name))
        .map(str::to_owned)
}

fn git(root: &Path, args: &[&str]) -> Result<String, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| Error::Git(format!("git {}: {error}", args.join(" "))))?;
    if !output.status.success() {
        return Err(Error::Git(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| Error::Git(format!("git {}: {error}", args.join(" "))))
}

/// The repository at a commit.
struct Git<'a> {
    root: &'a Path,
    commit: String,
}

impl<'a> Git<'a> {
    fn at(root: &'a Path, reference: &str) -> Result<Self, Error> {
        let commit = git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{reference}^{{commit}}"),
            ],
        )?;
        Ok(Git {
            root,
            commit: commit.trim().to_owned(),
        })
    }

    /// A file's text at the commit, or `None` where the commit has no such file.
    fn show(&self, file: &str) -> Result<Option<String>, Error> {
        let listed = git(
            self.root,
            &["ls-tree", "--name-only", &self.commit, "--", file],
        )?;
        if listed.trim().is_empty() {
            return Ok(None);
        }
        git(self.root, &["show", &format!("{}:{file}", self.commit)]).map(Some)
    }

    fn files_named(&self, name: &str) -> Result<Vec<String>, Error> {
        let listed = git(self.root, &["ls-tree", "-r", "--name-only", &self.commit])?;
        Ok(named(&listed, name).collect())
    }
}

/// The Cargo workspace's version at the previous release.
fn previous_version(git: &Git) -> Result<Version, Error> {
    let text = git
        .show("Cargo.toml")?
        .ok_or_else(|| Error::Git(format!("{} has no Cargo.toml", git.commit)))?;
    let path = Path::new("Cargo.toml");
    parse_toml(path, &text)?
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml_edit::Item::as_str)
        .ok_or_else(|| Error::Git(format!("{}'s Cargo.toml states no version", git.commit)))?
        .parse()
}
