//! A release's notes are its section of the changelog.

use std::path::Path;

use clerkenwell_release::{release_notes, Error, Version, CHANGELOG};

fn version(text: &str) -> Version {
    text.parse().expect("a version")
}

const CHANGELOG_TEXT: &str = "\
# Changelog

Notes for each release.

## 0.2.0 (2026-11-01)

### Breaking

- A record changed.

### Fixed

- A defect.

## 0.1.1

A patch.

## 0.1.0

The first release.
";

fn changelog(text: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(root.path().join(CHANGELOG), text).expect("writable");
    root
}

fn notes(root: &Path, text: &str) -> Result<String, Error> {
    release_notes(root, version(text))
}

#[test]
fn a_release_s_notes_are_its_section_without_the_heading() {
    let root = changelog(CHANGELOG_TEXT);
    assert_eq!(
        notes(root.path(), "0.2.0").expect("notes"),
        "### Breaking\n\n- A record changed.\n\n### Fixed\n\n- A defect.\n"
    );
    assert_eq!(notes(root.path(), "0.1.1").expect("notes"), "A patch.\n");
}

#[test]
fn the_last_section_runs_to_the_end_of_the_changelog() {
    let root = changelog(CHANGELOG_TEXT);
    assert_eq!(
        notes(root.path(), "0.1.0").expect("notes"),
        "The first release.\n"
    );
}

#[test]
fn a_version_without_a_section_is_refused() {
    let root = changelog(CHANGELOG_TEXT);
    for absent in ["0.3.0", "0.1.2", "1.0.0"] {
        assert!(
            matches!(notes(root.path(), absent), Err(Error::NoNotes { .. })),
            "{absent}"
        );
    }
}

#[test]
fn a_heading_names_a_version_only_as_its_first_word() {
    let root = changelog("# Changelog\n\n## 0.1.0-rc.1\n\nA candidate.\n\n## 10.1.0\n\nLater.\n");
    for absent in ["0.1.0", "1.0.0"] {
        assert!(
            matches!(notes(root.path(), absent), Err(Error::NoNotes { .. })),
            "{absent}"
        );
    }
}

#[test]
fn an_empty_section_is_refused() {
    let root = changelog("# Changelog\n\n## 0.2.0\n\n\n## 0.1.0\n\nThe first release.\n");
    assert!(matches!(
        notes(root.path(), "0.2.0"),
        Err(Error::NoNotes { .. })
    ));
}
