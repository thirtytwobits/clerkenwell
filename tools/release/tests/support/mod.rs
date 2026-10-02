//! The fixture workspace the tests copy, and helpers over a copy.

use std::path::Path;

use clerkenwell_release::Version;

/// The version every place in the fixture states.
pub const FIXTURE_VERSION: &str = "0.3.0";

pub fn version(text: &str) -> Version {
    text.parse().expect("a version")
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a directory");
    for entry in std::fs::read_dir(from).expect("the fixture is readable") {
        let entry = entry.expect("an entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a type").is_dir() {
            copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("a copy");
        }
    }
}

/// A fresh copy of the fixture workspace.
pub fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("a temporary directory");
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace"),
        directory.path(),
    );
    directory
}

/// Replaces the first `from` in `file` with `to`.
pub fn edit(root: &Path, file: &str, from: &str, to: &str) {
    let path = root.join(file);
    let text = std::fs::read_to_string(&path).expect("readable");
    assert!(text.contains(from), "{file} contains {from}");
    std::fs::write(&path, text.replacen(from, to, 1)).expect("writable");
}
