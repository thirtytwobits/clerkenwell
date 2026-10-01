//! The generator configuration: where the definition and the five artefacts
//! live, and every project-specific name the generated code carries.

use std::path::{Path, PathBuf};

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
    pub normalized_definition: PathBuf,
    pub typescript_model: PathBuf,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConfigFile {
    definition: PathBuf,
    outputs: OutputPaths,
    regenerate_command: String,
    #[serde(default)]
    header: Vec<String>,
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
                normalized_definition: resolve(outputs.normalized_definition),
                typescript_model: resolve(outputs.typescript_model),
                rust_model: resolve(outputs.rust_model),
            },
            project: Project {
                regenerate_command: file.regenerate_command,
                header: file.header,
            },
        })
    }
}
