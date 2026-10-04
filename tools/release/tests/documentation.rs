//! The documentation index links every workspace library's documentation and
//! states the version, and refuses documentation that lacks a library or
//! names Loro.

// These tests use only the fixture from the shared support module.
#[allow(dead_code)]
mod support;

use std::path::Path;

use clerkenwell_release::{index_documentation, Error};
use support::{fixture, FIXTURE_VERSION};

/// Writes `content` to `path` under `doc`, creating its directories.
fn page(doc: &Path, path: &str, content: &str) {
    let path = doc.join(path);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(path, content).expect("writable");
}

/// A documentation tree holding each fixture library's index page.
fn documented() -> tempfile::TempDir {
    let doc = tempfile::tempdir().expect("a temporary directory");
    page(doc.path(), "alpha/index.html", "<h1>alpha</h1>");
    page(doc.path(), "beta/index.html", "<h1>beta</h1>");
    doc
}

#[test]
fn the_index_links_each_library_and_states_the_version() {
    let root = fixture();
    let doc = documented();

    index_documentation(root.path(), doc.path()).expect("an index");

    let index = std::fs::read_to_string(doc.path().join("index.html")).expect("the index");
    assert!(index.contains("href=\"alpha/index.html\""));
    assert!(index.contains("href=\"beta/index.html\""));
    assert!(index.contains(FIXTURE_VERSION));
}

#[test]
fn documentation_lacking_a_library_is_refused() {
    let root = fixture();
    let doc = tempfile::tempdir().expect("a temporary directory");
    page(doc.path(), "alpha/index.html", "<h1>alpha</h1>");

    let error = index_documentation(root.path(), doc.path()).expect_err("beta is undocumented");

    assert!(matches!(error, Error::Undocumented { crates, .. } if crates == ["beta"]));
    assert!(!doc.path().join("index.html").exists());
}

#[test]
fn an_item_named_after_loro_is_refused() {
    let root = fixture();
    let doc = documented();
    page(
        doc.path(),
        "alpha/struct.LoroReplica.html",
        "<h1>LoroReplica</h1>",
    );

    let error = index_documentation(root.path(), doc.path()).expect_err("an item named after Loro");

    assert!(
        matches!(&error, Error::NamesLoro(paths) if paths.iter().any(|path| path.contains("LoroReplica")))
    );
}

#[test]
fn a_signature_naming_a_loro_type_is_refused() {
    let root = fixture();
    let doc = documented();
    page(
        doc.path(),
        "alpha/struct.Replica.html",
        "<a title=\"struct loro::LoroDoc\">LoroDoc</a>",
    );

    let error = index_documentation(root.path(), doc.path()).expect_err("a Loro type");

    assert!(
        matches!(&error, Error::NamesLoro(paths) if paths.iter().any(|path| path.contains("struct.Replica.html")))
    );
}

#[test]
fn source_pages_are_not_documentation() {
    let root = fixture();
    let doc = documented();
    page(doc.path(), "src/alpha/lib.rs.html", "use loro::LoroDoc;");

    index_documentation(root.path(), doc.path()).expect("an index");
}
