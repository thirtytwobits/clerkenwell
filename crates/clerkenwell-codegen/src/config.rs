//! The generator configuration: where the definition and the seven artefacts
//! live, and every project-specific name the generated code carries.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// A loaded configuration, its paths resolved against the configuration file's
/// directory.
#[derive(Clone, Debug)]
pub struct Config {
    pub definition: PathBuf,
    pub outputs: OutputPaths,
    pub project: Project,
}

/// Where each generated artefact is written.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutputPaths {
    pub collaboration_fixtures: PathBuf,
    pub contract_fixtures: PathBuf,
    pub coverage_report: PathBuf,
    pub normalized_definition: PathBuf,
    pub typescript_model: PathBuf,
    pub typescript_mnemonic: PathBuf,
    pub rust_model: PathBuf,
}

/// The project-specific names the generator writes into its artefacts.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Project {
    /// The command that regenerates the artefacts.
    pub regenerate_command: String,
    /// Lines opening every generated code file's header comment.
    #[serde(default)]
    pub header: Vec<String>,
    /// The mnemonic entry holding authoring session state.
    #[serde(default)]
    pub authoring_session_mnemonic: Option<String>,
    /// `$defs` entries a collaboration field stores whole.
    #[serde(default)]
    pub collaboration_leaf_schemas: Vec<String>,
    /// Coverage report diagnostics, by entity.
    #[serde(default)]
    pub entity_diagnostics: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConfigFile {
    definition: PathBuf,
    outputs: OutputPaths,
    regenerate_command: String,
    #[serde(default)]
    header: Vec<String>,
    #[serde(default)]
    authoring_session_mnemonic: Option<String>,
    #[serde(default)]
    collaboration_leaf_schemas: Vec<String>,
    #[serde(default)]
    entity_diagnostics: BTreeMap<String, Vec<String>>,
}

impl Config {
    /// Reads the configuration file at `path`.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        let file: ConfigFile = serde_json::from_str(&text).map_err(|source| Error::Config {
            path: path.to_owned(),
            message: source.to_string(),
        })?;
        let refuse = |message: String| Error::Config {
            path: path.to_owned(),
            message,
        };
        if file.regenerate_command.trim().is_empty() {
            return Err(refuse(
                "regenerateCommand must name the command that regenerates the outputs".to_owned(),
            ));
        }
        // Both are written into a line comment and a block comment.
        let breaks_comment = |text: &str| text.contains(['\n', '\r']) || text.contains("*/");
        if breaks_comment(&file.regenerate_command) {
            return Err(refuse(
                "regenerateCommand must be one line without \"*/\"".to_owned(),
            ));
        }
        if let Some(line) = file.header.iter().find(|line| breaks_comment(line)) {
            return Err(refuse(format!(
                "header line {line:?} must be one line without \"*/\""
            )));
        }
        let base = path.parent().unwrap_or(Path::new(""));
        let resolve = |relative: PathBuf| base.join(relative);
        let outputs = file.outputs;
        Ok(Config {
            definition: resolve(file.definition),
            outputs: OutputPaths {
                collaboration_fixtures: resolve(outputs.collaboration_fixtures),
                contract_fixtures: resolve(outputs.contract_fixtures),
                coverage_report: resolve(outputs.coverage_report),
                normalized_definition: resolve(outputs.normalized_definition),
                typescript_model: resolve(outputs.typescript_model),
                typescript_mnemonic: resolve(outputs.typescript_mnemonic),
                rust_model: resolve(outputs.rust_model),
            },
            project: Project {
                regenerate_command: file.regenerate_command,
                header: file.header,
                authoring_session_mnemonic: file.authoring_session_mnemonic,
                collaboration_leaf_schemas: file.collaboration_leaf_schemas,
                entity_diagnostics: file.entity_diagnostics,
            },
        })
    }

    /// The module specifier the mnemonic module imports the model module by.
    pub fn typescript_model_specifier(&self) -> Result<String> {
        relative_module_specifier(
            &self.outputs.typescript_mnemonic,
            &self.outputs.typescript_model,
        )
    }
}

/// Lexically normalised components of `path`: `.` dropped, `..` applied.
fn normalised(path: &Path) -> Vec<Component<'_>> {
    let mut components: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if matches!(components.last(), Some(Component::Normal(_))) => {
                components.pop();
            }
            other => components.push(other),
        }
    }
    components
}

/// An ECMAScript relative module specifier from the file `from` to the file
/// `to`, without `to`'s extension.
fn relative_module_specifier(from: &Path, to: &Path) -> Result<String> {
    let from = normalised(from);
    let to = normalised(to);
    let refuse = |message: String| Error::Config {
        path: PathBuf::from_iter(&from),
        message,
    };
    let (Some((_, from_directory)), Some((to_file, to_directory))) =
        (from.split_last(), to.split_last())
    else {
        return Err(refuse(
            "the TypeScript output paths must name files".to_owned(),
        ));
    };
    let shared = from_directory
        .iter()
        .zip(to_directory)
        .take_while(|(left, right)| left == right)
        .count();
    let unreachable = from_directory[shared..]
        .iter()
        .chain(&to_directory[shared..])
        .chain([to_file])
        .any(|component| !matches!(component, Component::Normal(_)));
    if unreachable {
        return Err(refuse(format!(
            "the TypeScript mnemonic module cannot import {} by a relative path",
            PathBuf::from_iter(&to).display()
        )));
    }
    let ascend = from_directory.len() - shared;
    let mut segments: Vec<String> = std::iter::repeat_n("..".to_owned(), ascend).collect();
    if ascend == 0 {
        segments.push(".".to_owned());
    }
    segments.extend(
        to_directory[shared..]
            .iter()
            .map(|component| component.as_os_str().to_string_lossy().into_owned()),
    );
    let file = Path::new(to_file.as_os_str());
    segments.push(
        file.file_stem()
            .unwrap_or(file.as_os_str())
            .to_string_lossy()
            .into_owned(),
    );
    Ok(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sibling_module_is_imported_from_its_own_directory() {
        let specifier = relative_module_specifier(
            Path::new("out/ts/mnemonic.ts"),
            Path::new("out/ts/index.ts"),
        )
        .unwrap();
        assert_eq!(specifier, "./index");
    }

    #[test]
    fn a_module_elsewhere_is_imported_through_the_shared_directory() {
        let specifier = relative_module_specifier(
            Path::new("out/./mnemonic/state.ts"),
            Path::new("out/model/../types/model.ts"),
        )
        .unwrap();
        assert_eq!(specifier, "../types/model");
    }
}
