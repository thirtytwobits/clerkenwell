//! The notebook example definition, and ways to vary it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use clerkenwell_codegen::json::{Json, Object};
use clerkenwell_codegen::{Config, Definition, Error, Result};

/// The directory holding the notebook definition, its configuration and its
/// committed outputs.
pub fn notebook_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../clerkenwell-notebook")
}

pub fn notebook_config_path() -> PathBuf {
    notebook_dir().join("clerkenwell-codegen.json")
}

pub fn notebook_config() -> Config {
    Config::load(&notebook_config_path()).expect("the notebook configuration loads")
}

/// The notebook definition document as parsed, before validation.
pub fn notebook_document() -> Json {
    let text = std::fs::read_to_string(notebook_dir().join("notebook.projections.json"))
        .expect("the notebook definition is readable");
    Json::parse(&text).expect("the notebook definition is JSON")
}

pub fn json(text: &str) -> Json {
    Json::parse(text).expect("test JSON parses")
}

/// Validates `document` with the notebook configuration's project names.
pub fn validate(document: Json) -> Result<Definition> {
    Definition::from_json(document, &notebook_config().project)
}

/// The message `document` is refused with.
pub fn refusal(document: Json) -> String {
    match validate(document) {
        Err(Error::Definition(message)) => message,
        Err(other) => panic!("expected a definition refusal, got {other}"),
        Ok(_) => panic!("expected the definition to be refused"),
    }
}

fn parent_and_key(pointer: &str) -> (&str, String) {
    let (parent, key) = pointer.rsplit_once('/').expect("a pointer below the root");
    (parent, key.replace("~1", "/").replace("~0", "~"))
}

fn object_at<'a>(document: &'a mut Json, pointer: &str) -> &'a mut Object {
    document
        .pointer_mut(pointer)
        .and_then(Json::as_object_mut)
        .unwrap_or_else(|| panic!("{pointer} is an object"))
}

/// Assigns `value` at `pointer`, whose parent must be an object.
pub fn set(document: &mut Json, pointer: &str, value: Json) {
    let (parent, key) = parent_and_key(pointer);
    object_at(document, parent).insert(key, value);
}

/// Removes the property at `pointer`.
pub fn remove(document: &mut Json, pointer: &str) {
    let (parent, key) = parent_and_key(pointer);
    object_at(document, parent)
        .remove(&key)
        .unwrap_or_else(|| panic!("{pointer} exists"));
}

/// Merges `patch`'s properties into the object at `pointer`.
pub fn merge(document: &mut Json, pointer: &str, patch: Json) {
    let patch = patch.as_object().expect("a patch is an object").clone();
    object_at(document, pointer).spread(&patch);
}

/// Appends `value` to the array at `pointer`.
pub fn push(document: &mut Json, pointer: &str, value: Json) {
    match document.pointer_mut(pointer) {
        Some(Json::Array(items)) => items.push(value),
        _ => panic!("{pointer} is an array"),
    }
}

/// The notebook definition with the `$defs` entries in `entries` added or replaced.
pub fn with_defs(entries: &str) -> Json {
    let mut document = notebook_document();
    merge(&mut document, "/$defs", json(entries));
    document
}

/// Copies the notebook definition, configuration and committed outputs into a
/// fresh directory, so a test can change them.
pub fn copy_notebook() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("a temporary directory");
    copy_tree(&notebook_dir(), directory.path());
    directory
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("the copy's directory is created");
    for entry in std::fs::read_dir(from).expect("the notebook directory is readable") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a file copies");
        }
    }
}
