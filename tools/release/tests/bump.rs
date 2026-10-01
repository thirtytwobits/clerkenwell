//! Reading, checking and moving the version of a workspace that states it in
//! a Cargo workspace, its members, npm workspace packages and both lockfiles.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clerkenwell_release::{agreed, bump, statements, Error, Version};

const FIXTURE_VERSION: &str = "0.3.0";

fn version(text: &str) -> Version {
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
fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("a temporary directory");
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace"),
        directory.path(),
    );
    directory
}

/// Every file in a workspace and its text.
fn files(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, String>) {
        for entry in std::fs::read_dir(directory).expect("readable") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "target") {
                    walk(root, &path, files);
                }
            } else {
                let text = std::fs::read_to_string(&path).expect("text");
                files.insert(path.strip_prefix(root).expect("inside").to_owned(), text);
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

fn edit(root: &Path, file: &str, from: &str, to: &str) {
    let path = root.join(file);
    let text = std::fs::read_to_string(&path).expect("readable");
    assert!(text.contains(from), "{file} contains {from}");
    std::fs::write(&path, text.replacen(from, to, 1)).expect("writable");
}

fn disagreeing(root: &Path) -> Vec<String> {
    match agreed(root) {
        Err(Error::Disagreement { disagreeing, .. }) => disagreeing
            .into_iter()
            .map(|statement| statement.place)
            .collect(),
        other => panic!("a disagreement, not {other:?}"),
    }
}

#[test]
fn every_place_that_states_the_version_is_read() {
    let workspace = fixture();
    let places: Vec<String> = statements(workspace.path())
        .expect("the fixture is readable")
        .into_iter()
        .map(|statement| statement.place)
        .collect();
    assert_eq!(
        places,
        [
            "Cargo.toml workspace.package.version",
            "crates/alpha/Cargo.toml package.version",
            "crates/beta/Cargo.toml package.version",
            "Cargo.lock alpha",
            "Cargo.lock beta",
            "packages/one/package.json version",
            "packages/two/package.json version",
            "packages/two/package.json dependencies.@fixture/one",
            "package-lock.json packages.packages/one.version",
            "package-lock.json packages.packages/two.version",
            "package-lock.json packages.packages/two.dependencies.@fixture/one",
        ]
    );
    assert_eq!(
        agreed(workspace.path()).expect("the fixture agrees"),
        version(FIXTURE_VERSION)
    );
}

#[test]
fn a_bump_moves_every_place_to_the_new_version() {
    let workspace = fixture();
    let bumped = bump(workspace.path(), version("0.4.0")).expect("a bump");
    assert_eq!(bumped, version("0.4.0"));
    for statement in statements(workspace.path()).expect("readable") {
        assert_eq!(statement.version, "0.4.0", "{}", statement.place);
    }
}

#[test]
fn a_bump_changes_nothing_but_the_version() {
    let workspace = fixture();
    let before = files(workspace.path());
    bump(workspace.path(), version("0.4.0")).expect("a bump");
    let after = files(workspace.path());
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (path, text) in after {
        assert_eq!(
            text.replace("0.4.0", FIXTURE_VERSION),
            before[&path],
            "{}",
            path.display()
        );
    }
}

#[test]
fn a_bump_leaves_other_packages_at_the_same_version_alone() {
    let workspace = fixture();
    bump(workspace.path(), version("0.4.0")).expect("a bump");
    let after = files(workspace.path());
    let unrelated = |file: &str| {
        after[Path::new(file)]
            .lines()
            .filter(|line| line.contains("unrelated") && line.contains(FIXTURE_VERSION))
            .count()
    };
    assert_eq!(unrelated("Cargo.toml"), 1);
    assert_eq!(unrelated("packages/one/package.json"), 1);
    assert_eq!(unrelated("packages/two/package.json"), 1);
    assert_eq!(unrelated("package-lock.json"), 3);
    assert!(after[Path::new("Cargo.lock")].contains("name = \"unrelated\"\nversion = \"0.3.0\""));
    assert!(after[Path::new("package-lock.json")]
        .contains("\"node_modules/unrelated\": {\n      \"version\": \"0.3.0\""));
}

#[test]
fn a_bump_to_the_current_version_changes_no_file() {
    let workspace = fixture();
    let before = files(workspace.path());
    bump(workspace.path(), version(FIXTURE_VERSION)).expect("a bump");
    assert_eq!(files(workspace.path()), before);
}

#[test]
fn a_lower_version_is_refused_and_changes_no_file() {
    let workspace = fixture();
    let before = files(workspace.path());
    let refused = bump(workspace.path(), version("0.2.9"));
    assert!(
        matches!(refused, Err(Error::Downgrade { .. })),
        "{refused:?}"
    );
    assert_eq!(files(workspace.path()), before);
}

#[test]
fn each_place_that_disagrees_is_reported() {
    let cases = [
        (
            "crates/beta/Cargo.toml",
            "version.workspace = true",
            "version = \"0.2.9\"",
            "crates/beta/Cargo.toml package.version",
        ),
        (
            "Cargo.lock",
            "name = \"alpha\"\nversion = \"0.3.0\"",
            "name = \"alpha\"\nversion = \"0.2.9\"",
            "Cargo.lock alpha",
        ),
        (
            "packages/one/package.json",
            "\"version\": \"0.3.0\"",
            "\"version\": \"0.2.9\"",
            "packages/one/package.json version",
        ),
        (
            "packages/two/package.json",
            "\"@fixture/one\": \"0.3.0\"",
            "\"@fixture/one\": \"^0.3.0\"",
            "packages/two/package.json dependencies.@fixture/one",
        ),
        (
            "package-lock.json",
            "\"@fixture/two\",\n      \"version\": \"0.3.0\"",
            "\"@fixture/two\",\n      \"version\": \"0.2.9\"",
            "package-lock.json packages.packages/two.version",
        ),
    ];
    for (file, from, to, place) in cases {
        let workspace = fixture();
        edit(workspace.path(), file, from, to);
        assert_eq!(disagreeing(workspace.path()), [place]);
    }
}

#[test]
fn a_bump_brings_every_disagreeing_place_into_line() {
    let workspace = fixture();
    edit(
        workspace.path(),
        "crates/beta/Cargo.toml",
        "version.workspace = true",
        "version = \"0.2.9\"",
    );
    edit(
        workspace.path(),
        "packages/two/package.json",
        "\"@fixture/one\": \"0.3.0\"",
        "\"@fixture/one\": \"^0.3.0\"",
    );
    bump(workspace.path(), version("0.4.0")).expect("a bump");
    assert_eq!(
        agreed(workspace.path()).expect("every place agrees"),
        version("0.4.0")
    );
}

#[test]
fn an_npm_workspace_pattern_is_refused() {
    let workspace = fixture();
    edit(
        workspace.path(),
        "package.json",
        "\"packages/one\",\n    \"packages/two\"",
        "\"packages/*\"",
    );
    let refused = statements(workspace.path());
    assert!(
        matches!(refused, Err(Error::WorkspacePattern(_))),
        "{refused:?}"
    );
}

#[test]
fn a_version_is_three_decimal_numbers() {
    for text in ["0.1.0", "1.0.0", "10.20.30"] {
        assert_eq!(version(text).to_string(), text);
    }
    for text in [
        "",
        "1.2",
        "1.2.3.4",
        "v1.2.3",
        "01.2.3",
        "1.02.3",
        "1.2.3-rc.1",
        "1..3",
        "1.2.x",
    ] {
        assert!(
            matches!(text.parse::<Version>(), Err(Error::InvalidVersion(_))),
            "{text:?} is refused"
        );
    }
}

#[test]
fn versions_order_by_number() {
    assert!(version("0.10.0") > version("0.9.9"));
    assert!(version("1.0.0") > version("0.99.99"));
    assert!(version("0.1.10") > version("0.1.9"));
}
