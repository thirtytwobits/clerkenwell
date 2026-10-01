//! The one version every Clerkenwell crate and package is released at.
//!
//! The Cargo workspace states the version in `[workspace.package]`, which each
//! member inherits. Each npm workspace package states it in its
//! `package.json`, as does every dependency one workspace package declares on
//! another. `Cargo.lock` and `package-lock.json` record each of those again.
//! [`statements`] reads every one, [`agreed`] requires them to agree, and
//! [`bump`] moves them all to a new version, changing nothing else in any file.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use jsonc_parser::ParseOptions;
use toml_edit::DocumentMut;

/// The sections of a `package.json` that name a package's dependencies.
const DEPENDENCY_SECTIONS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// The command that checks every statement agrees.
pub const CHECK_COMMAND: &str = "cargo run -p clerkenwell-release -- check";

/// This Clerkenwell checkout.
pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A release version: `MAJOR.MINOR.PATCH`, each a decimal number without
/// leading zeros.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl FromStr for Version {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        let invalid = || Error::InvalidVersion(text.to_owned());
        let numbers = text
            .split('.')
            .map(|part| {
                let digits = !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
                if !digits || (part.len() > 1 && part.starts_with('0')) {
                    return Err(invalid());
                }
                part.parse::<u64>().map_err(|_| invalid())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let [major, minor, patch] = numbers[..] else {
            return Err(invalid());
        };
        Ok(Version {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One place that states the version, and the version it states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The file, relative to the workspace, and the place in it.
    pub place: String,
    pub version: String,
}

impl fmt::Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.place, self.version)
    }
}

/// Why the version could not be read, checked or moved.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0:?} is not a version: write MAJOR.MINOR.PATCH")]
    InvalidVersion(String),
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} does not parse: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{path} has no {what}")]
    Missing { path: PathBuf, what: String },
    #[error("cargo metadata fails: {0}")]
    Metadata(String),
    #[error("npm workspace {0:?} is a pattern: list each package's directory")]
    WorkspacePattern(String),
    #[error("{} states {expected}; these disagree:\n{}", WORKSPACE_PLACE, list(.disagreeing))]
    Disagreement {
        expected: String,
        disagreeing: Vec<Statement>,
    },
    #[error("{requested} is lower than the workspace's version, {current}")]
    Downgrade {
        requested: Version,
        current: Version,
    },
}

fn list(statements: &[Statement]) -> String {
    statements
        .iter()
        .map(|statement| format!("  {statement}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where the Cargo workspace states the version every member inherits.
const WORKSPACE_PLACE: &str = "Cargo.toml workspace.package.version";

/// Every place under `root` that states the version, the Cargo workspace's
/// first.
pub fn statements(root: &Path) -> Result<Vec<Statement>, Error> {
    survey(root, None)
}

/// The version every place under `root` states, or every place that
/// disagrees with the Cargo workspace's.
pub fn agreed(root: &Path) -> Result<Version, Error> {
    let statements = statements(root)?;
    let (workspace, others) = statements
        .split_first()
        .expect("the survey starts with the Cargo workspace");
    let disagreeing: Vec<Statement> = others
        .iter()
        .filter(|statement| statement.version != workspace.version)
        .cloned()
        .collect();
    if !disagreeing.is_empty() {
        return Err(Error::Disagreement {
            expected: workspace.version.clone(),
            disagreeing,
        });
    }
    workspace.version.parse()
}

/// Moves every place under `root` to `version` and returns it once every
/// place agrees. A version lower than the Cargo workspace's is refused.
pub fn bump(root: &Path, version: Version) -> Result<Version, Error> {
    let current: Version = cargo_workspace(root, None)?.version.parse()?;
    if version < current {
        return Err(Error::Downgrade {
            requested: version,
            current,
        });
    }
    survey(root, Some(version))?;
    agreed(root)
}

/// Reads every statement, and with `set`, writes `set` in its place. Returns
/// the statements as they were read.
fn survey(root: &Path, set: Option<Version>) -> Result<Vec<Statement>, Error> {
    let root = root.canonicalize().map_err(|source| Error::Read {
        path: root.to_owned(),
        source,
    })?;
    let mut statements = vec![cargo_workspace(&root, set)?];
    let members = cargo_members(&root)?;
    for member in &members {
        statements.push(cargo_member(&root, member, set)?);
    }
    statements.extend(cargo_lock(&root, &members, set)?);
    let packages = npm_packages(&root)?;
    for package in &packages {
        statements.extend(npm_manifest(&root, package, &packages, set)?);
    }
    statements.extend(npm_lock(&root, &packages, set)?);
    Ok(statements)
}

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })
}

/// Writes `text` to `path` when it differs from `original`.
fn write(path: &Path, original: &str, text: &str) -> Result<(), Error> {
    if text == original {
        return Ok(());
    }
    std::fs::write(path, text).map_err(|source| Error::Write {
        path: path.to_owned(),
        source,
    })
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn missing(path: &Path, what: impl Into<String>) -> Error {
    Error::Missing {
        path: path.to_owned(),
        what: what.into(),
    }
}

fn parse_toml(path: &Path, text: &str) -> Result<DocumentMut, Error> {
    text.parse()
        .map_err(|error: toml_edit::TomlError| Error::Parse {
            path: path.to_owned(),
            message: error.to_string(),
        })
}

/// Replaces a TOML string with `version`, keeping the whitespace and
/// comments around it.
fn set_toml(value: &mut toml_edit::Value, version: Version) {
    let decor = value.decor().clone();
    *value = version.to_string().into();
    *value.decor_mut() = decor;
}

/// `[workspace.package] version` in the root `Cargo.toml`.
fn cargo_workspace(root: &Path, set: Option<Version>) -> Result<Statement, Error> {
    let path = root.join("Cargo.toml");
    let text = read(&path)?;
    let mut document = parse_toml(&path, &text)?;
    let value = document
        .get_mut("workspace")
        .and_then(|workspace| workspace.get_mut("package"))
        .and_then(|package| package.get_mut("version"))
        .and_then(toml_edit::Item::as_value_mut)
        .filter(|value| value.is_str())
        .ok_or_else(|| missing(&path, "workspace.package.version string"))?;
    let statement = Statement {
        place: WORKSPACE_PLACE.to_owned(),
        version: value.as_str().unwrap_or_default().to_owned(),
    };
    if let Some(version) = set {
        set_toml(value, version);
        write(&path, &text, &document.to_string())?;
    }
    Ok(statement)
}

/// A Cargo workspace member.
struct Member {
    name: String,
    version: String,
    manifest: PathBuf,
}

fn cargo_members(root: &Path) -> Result<Vec<Member>, Error> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .output()
        .map_err(|error| Error::Metadata(error.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Metadata(stderr.trim().to_owned()));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::Metadata(error.to_string()))?;
    let field = |package: &serde_json::Value, name: &str| {
        package[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Metadata(format!("a package without a {name}")))
    };
    metadata["packages"]
        .as_array()
        .ok_or_else(|| Error::Metadata("no packages".to_owned()))?
        .iter()
        .map(|package| {
            Ok(Member {
                name: field(package, "name")?,
                version: field(package, "version")?,
                manifest: PathBuf::from(field(package, "manifest_path")?),
            })
        })
        .collect()
}

/// A member's version as Cargo resolves it. A member that states its own
/// version has it rewritten; one that inherits the workspace's follows it.
fn cargo_member(root: &Path, member: &Member, set: Option<Version>) -> Result<Statement, Error> {
    let place = relative(root, &member.manifest);
    let statement = Statement {
        place: format!("{place} package.version"),
        version: member.version.clone(),
    };
    if let Some(version) = set {
        let text = read(&member.manifest)?;
        let mut document = parse_toml(&member.manifest, &text)?;
        let stated = document
            .get_mut("package")
            .and_then(|package| package.get_mut("version"))
            .and_then(toml_edit::Item::as_value_mut)
            .filter(|value| value.is_str());
        if let Some(value) = stated {
            set_toml(value, version);
            write(&member.manifest, &text, &document.to_string())?;
        }
    }
    Ok(statement)
}

/// Each member's entry in `Cargo.lock`.
fn cargo_lock(
    root: &Path,
    members: &[Member],
    set: Option<Version>,
) -> Result<Vec<Statement>, Error> {
    let path = root.join("Cargo.lock");
    let text = read(&path)?;
    let mut document = parse_toml(&path, &text)?;
    let packages = document
        .get_mut("package")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .ok_or_else(|| missing(&path, "packages"))?;
    let mut statements = Vec::new();
    for member in members {
        let entry = packages
            .iter_mut()
            .find(|entry| {
                entry.get("source").is_none()
                    && entry.get("name").and_then(toml_edit::Item::as_str) == Some(&member.name)
            })
            .ok_or_else(|| missing(&path, format!("entry for {}", member.name)))?;
        let value = entry
            .get_mut("version")
            .and_then(toml_edit::Item::as_value_mut)
            .filter(|value| value.is_str())
            .ok_or_else(|| missing(&path, format!("version for {}", member.name)))?;
        statements.push(Statement {
            place: format!("Cargo.lock {}", member.name),
            version: value.as_str().unwrap_or_default().to_owned(),
        });
        if let Some(version) = set {
            set_toml(value, version);
        }
    }
    if set.is_some() {
        write(&path, &text, &document.to_string())?;
    }
    Ok(statements)
}

/// An npm workspace package.
struct Package {
    /// Its directory relative to the root, as the root `package.json` lists it.
    directory: String,
    name: String,
}

/// A JSON document and its top-level object.
fn parse_json(path: &Path, text: &str) -> Result<(CstRootNode, CstObject), Error> {
    let document =
        CstRootNode::parse(text, &ParseOptions::default()).map_err(|error| Error::Parse {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    let object = document
        .object_value()
        .ok_or_else(|| missing(path, "top-level object"))?;
    Ok((document, object))
}

fn json_string(object: &CstObject, key: &str) -> Option<String> {
    object
        .get(key)?
        .value()?
        .as_string_lit()?
        .decoded_value()
        .ok()
}

/// Reads the string at `key` in `object` as a statement, and with `set`,
/// replaces it.
fn json_statement(
    object: &CstObject,
    key: &str,
    path: &Path,
    place: String,
    set: Option<Version>,
) -> Result<Statement, Error> {
    let version =
        json_string(object, key).ok_or_else(|| missing(path, format!("{place} string")))?;
    if let (Some(version), Some(property)) = (set, object.get(key)) {
        property.set_value(CstInputValue::String(version.to_string()));
    }
    Ok(Statement { place, version })
}

/// The statements in one package entry: its version and each dependency on
/// another workspace package. `prefix` names the entry within its file.
fn package_statements(
    entry: &CstObject,
    path: &Path,
    prefix: &str,
    packages: &[Package],
    set: Option<Version>,
) -> Result<Vec<Statement>, Error> {
    let mut statements = vec![json_statement(
        entry,
        "version",
        path,
        format!("{prefix}version"),
        set,
    )?];
    for section in DEPENDENCY_SECTIONS {
        let Some(dependencies) = entry.object_value(section) else {
            continue;
        };
        for package in packages {
            if dependencies.get(&package.name).is_some() {
                let place = format!("{prefix}{section}.{}", package.name);
                statements.push(json_statement(
                    &dependencies,
                    &package.name,
                    path,
                    place,
                    set,
                )?);
            }
        }
    }
    Ok(statements)
}

fn npm_packages(root: &Path) -> Result<Vec<Package>, Error> {
    let path = root.join("package.json");
    let (_, manifest) = parse_json(&path, &read(&path)?)?;
    let workspaces = manifest
        .get("workspaces")
        .and_then(|property| property.value())
        .and_then(|value| value.as_array())
        .ok_or_else(|| missing(&path, "workspaces list"))?;
    workspaces
        .elements()
        .into_iter()
        .map(|element| {
            let directory = element
                .as_string_lit()
                .and_then(|literal| literal.decoded_value().ok())
                .ok_or_else(|| missing(&path, "workspace directory string"))?;
            if directory.contains('*') {
                return Err(Error::WorkspacePattern(directory));
            }
            let package_path = root.join(&directory).join("package.json");
            let name = json_string(&parse_json(&package_path, &read(&package_path)?)?.1, "name")
                .ok_or_else(|| missing(&package_path, "name string"))?;
            Ok(Package { directory, name })
        })
        .collect()
}

/// One workspace package's `package.json`.
fn npm_manifest(
    root: &Path,
    package: &Package,
    packages: &[Package],
    set: Option<Version>,
) -> Result<Vec<Statement>, Error> {
    let path = root.join(&package.directory).join("package.json");
    let text = read(&path)?;
    let (document, manifest) = parse_json(&path, &text)?;
    let prefix = format!("{} ", relative(root, &path));
    let statements = package_statements(&manifest, &path, &prefix, packages, set)?;
    write(&path, &text, &document.to_string())?;
    Ok(statements)
}

/// Each workspace package's entry in `package-lock.json`.
fn npm_lock(
    root: &Path,
    packages: &[Package],
    set: Option<Version>,
) -> Result<Vec<Statement>, Error> {
    let path = root.join("package-lock.json");
    let text = read(&path)?;
    let (document, lock) = parse_json(&path, &text)?;
    let entries = lock
        .object_value("packages")
        .ok_or_else(|| missing(&path, "packages"))?;
    let mut statements = Vec::new();
    for package in packages {
        let entry = entries
            .object_value(&package.directory)
            .ok_or_else(|| missing(&path, format!("entry for {}", package.directory)))?;
        let prefix = format!("package-lock.json packages.{}.", package.directory);
        statements.extend(package_statements(&entry, &path, &prefix, packages, set)?);
    }
    write(&path, &text, &document.to_string())?;
    Ok(statements)
}
