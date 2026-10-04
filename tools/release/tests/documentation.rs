//! The documentation site opens with the README, links every workspace
//! library's documentation and each Markdown document the README reaches, and
//! states the version. It refuses documentation that lacks a library or names
//! Loro, and a document that links a missing file.

// These tests use only the fixture from the shared support module.
#[allow(dead_code)]
mod support;

use std::path::Path;

use clerkenwell_release::{index_documentation, Error};

/// The repository the fixture workspace names.
const REPOSITORY: &str = "https://example.com/fixture";
use support::{fixture, FIXTURE_VERSION};

/// Writes `content` to `path` under `directory`, creating its directories.
fn page(directory: &Path, path: &str, content: &str) {
    let path = directory.join(path);
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

fn read(doc: &Path, page: &str) -> String {
    std::fs::read_to_string(doc.join(page)).expect("a page")
}

#[test]
fn the_front_page_is_the_readme_followed_by_each_library() {
    let root = fixture();
    let doc = documented();

    let site = index_documentation(root.path(), doc.path()).expect("a site");

    assert_eq!(site.pages, ["index.html"]);
    let index = read(doc.path(), "index.html");
    assert!(index.contains("A workspace of two libraries."));
    assert!(index.contains("href=\"alpha/index.html\""));
}

#[test]
fn a_document_the_readme_links_is_rendered_and_linked() {
    let root = fixture();
    let doc = documented();
    page(
        root.path(),
        "README.md",
        "# Fixture\n\nSee [the guide](docs/guide.md).\n",
    );
    page(
        root.path(),
        "docs/guide.md",
        "# Guide\n\nBack to [the start](../README.md#fixture).\n",
    );

    let site = index_documentation(root.path(), doc.path()).expect("a site");

    assert!(site.pages.iter().any(|page| page == "docs/guide.html"));
    assert!(read(doc.path(), "index.html").contains("href=\"docs/guide.html\""));
    let guide = read(doc.path(), "docs/guide.html");
    assert!(guide.contains("Back to"));
    assert!(guide.contains("href=\"../index.html#fixture\""));
    assert!(read(doc.path(), "index.html").contains("id=\"fixture\""));
    assert!(guide.contains(FIXTURE_VERSION));
}

#[test]
fn a_link_to_another_file_points_at_it_in_the_repository_at_the_release() {
    let root = fixture();
    let doc = documented();
    page(
        root.path(),
        "README.md",
        "# Fixture\n\nSee [the manifest](Cargo.toml).\n",
    );

    index_documentation(root.path(), doc.path()).expect("a site");

    let link = format!("href=\"{REPOSITORY}/blob/v{FIXTURE_VERSION}/Cargo.toml\"");
    assert!(read(doc.path(), "index.html").contains(&link));
}

#[test]
fn a_link_to_a_missing_file_is_refused() {
    let root = fixture();
    let doc = documented();
    page(
        root.path(),
        "README.md",
        "# Fixture\n\nSee [nothing](missing.md).\n",
    );

    let error = index_documentation(root.path(), doc.path()).expect_err("a broken link");

    assert!(matches!(error, Error::BrokenLink { link, .. } if link == "missing.md"));
    assert!(!doc.path().join("index.html").exists());
}

#[test]
fn a_link_leaving_the_workspace_is_refused() {
    let root = fixture();
    let doc = documented();
    page(
        root.path(),
        "README.md",
        "# Fixture\n\nSee [outside](../outside.md).\n",
    );

    let error = index_documentation(root.path(), doc.path()).expect_err("a link outside");

    assert!(matches!(error, Error::BrokenLink { .. }));
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
        matches!(&error, Error::ForbiddenNames(paths) if paths.iter().any(|path| path.contains("LoroReplica")))
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
        matches!(&error, Error::ForbiddenNames(paths) if paths.iter().any(|path| path.contains("struct.Replica.html")))
    );
}

#[test]
fn source_pages_are_not_documentation() {
    let root = fixture();
    let doc = documented();
    page(doc.path(), "src/alpha/lib.rs.html", "use loro::LoroDoc;");

    index_documentation(root.path(), doc.path()).expect("an index");
}

#[test]
fn a_document_naming_a_loro_path_is_refused_and_nothing_is_written() {
    let root = fixture();
    let doc = documented();
    page(
        root.path(),
        "README.md",
        "# Fixture\n\nBuilt on `loro::LoroDoc`.\n",
    );

    let error = index_documentation(root.path(), doc.path()).expect_err("a Loro path");

    assert!(
        matches!(&error, Error::ForbiddenNames(pages) if pages.iter().any(|page| page == "index.html"))
    );
    assert!(!doc.path().join("index.html").exists());
}

#[test]
fn an_item_documented_inside_its_parent_page_and_named_after_loro_is_refused() {
    let root = fixture();
    let doc = documented();
    page(
        doc.path(),
        "alpha/struct.Replica.html",
        "<section id=\"method.loro_replica\"><h4>fn loro_replica</h4></section>",
    );

    let error = index_documentation(root.path(), doc.path()).expect_err("a Loro-named method");

    assert!(
        matches!(&error, Error::ForbiddenNames(paths) if paths.iter().any(|path| path.contains("struct.Replica.html")))
    );
}

#[test]
fn each_heading_has_an_anchor_and_a_repeated_heading_a_numbered_one() {
    let root = fixture();
    let doc = documented();
    page(
        root.path(),
        "README.md",
        "# Fixture\n\nSee [the second use](docs/guide.md#usage-1).\n",
    );
    page(
        root.path(),
        "docs/guide.md",
        "# Guide\n\n## Usage\n\nFirst.\n\n## Usage\n\nSecond.\n",
    );

    index_documentation(root.path(), doc.path()).expect("a site");

    let guide = read(doc.path(), "docs/guide.html");
    assert!(guide.contains("id=\"usage\""));
    assert!(guide.contains("id=\"usage-1\""));
    assert!(read(doc.path(), "index.html").contains("href=\"docs/guide.html#usage-1\""));
}

#[test]
fn a_fragment_naming_no_heading_in_another_document_is_refused() {
    let root = fixture();
    let doc = documented();
    page(
        root.path(),
        "README.md",
        "# Fixture\n\nSee [the guide](docs/guide.md#absent).\n",
    );
    page(root.path(), "docs/guide.md", "# Guide\n");

    let error = index_documentation(root.path(), doc.path()).expect_err("a missing heading");

    assert!(matches!(error, Error::BrokenLink { link, .. } if link == "docs/guide.md#absent"));
}

#[test]
fn a_fragment_naming_no_heading_in_its_own_document_is_refused() {
    let root = fixture();
    let doc = documented();
    page(
        root.path(),
        "README.md",
        "# Fixture\n\nSee [below](#absent).\n",
    );

    let error = index_documentation(root.path(), doc.path()).expect_err("a missing heading");

    assert!(matches!(error, Error::BrokenLink { link, .. } if link == "#absent"));
}
