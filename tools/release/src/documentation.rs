//! The index page of the workspace's API documentation.
//!
//! `cargo doc --workspace --no-deps` writes each library's documentation to
//! its own directory and no page linking them. [`index_documentation`] writes
//! that page, stating the version, once it has checked that every library is
//! documented and that the documentation names no Loro item or type.

use std::path::Path;

use crate::{agreed, cargo_metadata, cargo_packages, Error, Version};

/// The command that builds the documentation [`index_documentation`] indexes.
pub const DOCUMENTATION_COMMAND: &str = "cargo doc --workspace --no-deps";

/// The directories at the top of a documentation tree that hold the crates'
/// source and rustdoc's assets, which document no item.
const UNDOCUMENTED: &[&str] = &["src", "static.files"];

/// The text rustdoc renders wherever documentation names a Loro path.
const LORO_PATH: &[u8] = b"loro::";

/// A workspace library, by the name rustdoc gives its documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentedCrate {
    pub name: String,
    pub description: Option<String>,
}

/// Writes `doc/index.html`, linking the documentation of each library in the
/// workspace under `root` and stating the version every manifest agrees on.
/// Refuses a tree that lacks a library's documentation or names Loro.
pub fn index_documentation(root: &Path, doc: &Path) -> Result<Vec<DocumentedCrate>, Error> {
    let version = agreed(root)?;
    let mut naming = Vec::new();
    for entry in entries(doc)? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !UNDOCUMENTED.contains(&name.as_str()) {
            find_loro(doc, &entry.path(), &mut naming)?;
        }
    }
    if !naming.is_empty() {
        naming.sort();
        return Err(Error::NamesLoro(naming));
    }
    let crates = libraries(root)?;
    let undocumented: Vec<String> = crates
        .iter()
        .filter(|library| !doc.join(&library.name).join("index.html").is_file())
        .map(|library| library.name.clone())
        .collect();
    if !undocumented.is_empty() {
        return Err(Error::Undocumented {
            doc: doc.to_owned(),
            crates: undocumented,
        });
    }
    let path = doc.join("index.html");
    std::fs::write(&path, page(version, &crates))
        .map_err(|source| Error::Write { path, source })?;
    Ok(crates)
}

/// Each library target in the workspace under `root`, in name order.
fn libraries(root: &Path) -> Result<Vec<DocumentedCrate>, Error> {
    let metadata = cargo_metadata(root)?;
    let mut crates: Vec<DocumentedCrate> = cargo_packages(&metadata)?
        .iter()
        .flat_map(|package| {
            let description = package["description"].as_str().map(str::to_owned);
            package["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|target| {
                    target["kind"]
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind == "lib"))
                })
                .filter_map(move |target| {
                    Some(DocumentedCrate {
                        name: target["name"].as_str()?.replace('-', "_"),
                        description: description.clone(),
                    })
                })
        })
        .collect();
    crates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(crates)
}

fn entries(directory: &Path) -> Result<Vec<std::fs::DirEntry>, Error> {
    let read = |source| Error::Read {
        path: directory.to_owned(),
        source,
    };
    std::fs::read_dir(directory)
        .map_err(read)?
        .collect::<Result<_, _>>()
        .map_err(read)
}

/// Adds to `naming` each path under `path` that is named after Loro or whose
/// content names a Loro path, relative to `doc`.
fn find_loro(doc: &Path, path: &Path, naming: &mut Vec<String>) -> Result<(), Error> {
    let relative = path
        .strip_prefix(doc)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let named = path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().to_lowercase().contains("loro"));
    if path.is_dir() {
        if named {
            naming.push(relative);
        }
        for entry in entries(path)? {
            find_loro(doc, &entry.path(), naming)?;
        }
        return Ok(());
    }
    let content = std::fs::read(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    if named
        || content
            .windows(LORO_PATH.len())
            .any(|window| window == LORO_PATH)
    {
        naming.push(relative);
    }
    Ok(())
}

fn page(version: Version, crates: &[DocumentedCrate]) -> String {
    let items: String = crates
        .iter()
        .map(|library| {
            let name = escape(&library.name);
            let description = library
                .description
                .as_deref()
                .map(|text| format!("<dd>{}</dd>", escape(text)))
                .unwrap_or_default();
            format!("<dt><a href=\"{name}/index.html\">{name}</a></dt>{description}\n")
        })
        .collect();
    format!(
        "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Clerkenwell {version} API documentation</title>
<style>
body {{ font-family: system-ui, sans-serif; max-width: 48rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.5; }}
dt {{ margin-top: 1rem; font-family: ui-monospace, monospace; }}
dd {{ margin-left: 0; }}
</style>
</head>
<body>
<h1>Clerkenwell {version}</h1>
<p>API documentation for each crate in release {version}.</p>
<dl>
{items}</dl>
</body>
</html>
"
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
