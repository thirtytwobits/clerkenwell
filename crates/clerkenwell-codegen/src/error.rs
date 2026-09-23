use std::path::PathBuf;

/// Everything that stops a generation or check run.
#[derive(Debug, thiserror::Error)]
pub enum Error {
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
    #[error("{path} is not valid JSON: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{path} is not a valid generator configuration: {message}")]
    Config { path: PathBuf, message: String },
    /// The definition is refused: invalid, or outside what the renderers can
    /// represent exactly.
    #[error("{0}")]
    Definition(String),
    #[error(
        "Generated projection bindings are stale. Run `{command}` to update:\n{}",
        .paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n")
    )]
    Stale {
        command: String,
        paths: Vec<PathBuf>,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Refuses the definition with `message`.
pub(crate) fn refuse<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Definition(message.into()))
}
