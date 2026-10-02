//! A release measured against the previous one: a patch release may not
//! change a contract.

use std::path::Path;
use std::process::Command;

use clerkenwell_release::{
    bump, check_release, resolve, workspace, Comparison, Contract, Error, Release, CONTRACTS,
};

mod support;

use support::{edit, fixture, version, FIXTURE_VERSION};

const PREVIOUS: &str = "v0.3.0";

const CONTRACTS_UNDER_TEST: &[Contract] = &[
    Contract::Records("public-api.txt"),
    Contract::Records("public-api.json"),
    Contract::File("schema/definition.schema.json"),
    Contract::Constant {
        file: "crates/alpha/src/lib.rs",
        name: "FORMAT_VERSION",
    },
];

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(path, text).expect("writable");
}

/// The fixture workspace with one file under each contract, committed and
/// tagged as the previous release.
fn released() -> tempfile::TempDir {
    let workspace = fixture();
    let root = workspace.path();
    write(root, "crates/alpha/public-api.txt", "fn alpha::one()\n");
    write(root, "crates/beta/public-api.txt", "fn beta::two()\n");
    write(
        root,
        "packages/one/public-api.json",
        "{\n  \".\": [\"one\"]\n}\n",
    );
    write(
        root,
        "schema/definition.schema.json",
        "{\n  \"type\": \"object\"\n}\n",
    );
    write(
        root,
        "crates/alpha/src/lib.rs",
        "pub const FORMAT_VERSION: u32 = 1;\n",
    );
    git(root, &["init", "--quiet"]);
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "--message", "release"]);
    git(root, &["tag", PREVIOUS]);
    workspace
}

fn check(root: &Path) -> Result<Comparison, Error> {
    check_release(root, PREVIOUS, CONTRACTS_UNDER_TEST)
}

/// A change to a workspace.
type Change = Box<dyn Fn(&Path)>;

/// A change to each contract, and the place it is reported at.
fn contract_changes() -> Vec<(&'static str, Change)> {
    vec![
        (
            "crates/beta/public-api.txt",
            Box::new(|root| edit(root, "crates/beta/public-api.txt", "two", "three")),
        ),
        (
            "packages/one/public-api.json",
            Box::new(|root| edit(root, "packages/one/public-api.json", "\"one\"", "\"uno\"")),
        ),
        (
            "packages/two/public-api.json",
            Box::new(|root| {
                write(
                    root,
                    "packages/two/public-api.json",
                    "{\n  \".\": [\"two\"]\n}\n",
                )
            }),
        ),
        (
            "crates/alpha/public-api.txt",
            Box::new(|root| {
                std::fs::remove_file(root.join("crates/alpha/public-api.txt")).expect("removable")
            }),
        ),
        (
            "schema/definition.schema.json",
            Box::new(|root| edit(root, "schema/definition.schema.json", "object", "array")),
        ),
        (
            "crates/alpha/src/lib.rs FORMAT_VERSION",
            Box::new(|root| edit(root, "crates/alpha/src/lib.rs", "= 1;", "= 2;")),
        ),
    ]
}

#[test]
fn a_patch_release_that_changes_no_contract_is_admitted() {
    let workspace = released();
    let root = workspace.path();
    write(root, "crates/beta/src/lib.rs", "pub fn fixed() {}\n");
    bump(root, version("0.3.1")).expect("a bump");
    assert_eq!(
        check(root).expect("admitted"),
        Comparison {
            previous: version(FIXTURE_VERSION),
            current: version("0.3.1"),
            release: Release::Patch,
            changed: Vec::new(),
        }
    );
}

#[test]
fn a_patch_release_that_changes_any_contract_is_refused() {
    for (place, change) in contract_changes() {
        let workspace = released();
        let root = workspace.path();
        change(root);
        bump(root, version("0.3.1")).expect("a bump");
        match check(root) {
            Err(Error::Breaking { changed, .. }) => assert_eq!(changed, [place]),
            other => panic!("{place}: a refusal, not {other:?}"),
        }
    }
}

#[test]
fn a_minor_or_major_release_may_change_contracts() {
    for (next, release) in [("0.4.0", Release::Minor), ("1.0.0", Release::Major)] {
        for (place, change) in contract_changes() {
            let workspace = released();
            let root = workspace.path();
            change(root);
            bump(root, version(next)).expect("a bump");
            let comparison = check(root).expect("admitted");
            assert_eq!(comparison.release, release, "{place}");
            assert_eq!(comparison.changed, [place]);
        }
    }
}

#[test]
fn a_comparison_reads_as_the_release_then_each_changed_contract() {
    let comparison = Comparison {
        previous: version("0.3.0"),
        current: version("0.4.0"),
        release: Release::Minor,
        changed: vec!["a/public-api.txt".to_owned(), "b/public-api.txt".to_owned()],
    };
    assert_eq!(
        comparison.to_string(),
        "0.3.0 to 0.4.0 is a minor release.\nchanged: a/public-api.txt\nchanged: b/public-api.txt"
    );
    let unchanged = Comparison {
        changed: Vec::new(),
        ..comparison
    };
    assert_eq!(unchanged.to_string(), "0.3.0 to 0.4.0 is a minor release.");
}

#[test]
fn a_change_beside_a_constant_is_not_a_contract_change() {
    let workspace = released();
    let root = workspace.path();
    edit(
        root,
        "crates/alpha/src/lib.rs",
        "pub const FORMAT_VERSION: u32 = 1;\n",
        "/// The format.\npub const FORMAT_VERSION: u32 = 1;\n\npub fn alpha() {}\n",
    );
    bump(root, version("0.3.1")).expect("a bump");
    assert_eq!(check(root).expect("admitted").changed, Vec::<String>::new());
}

#[test]
fn every_changed_contract_is_reported() {
    let workspace = released();
    let root = workspace.path();
    let mut places = Vec::new();
    for (place, change) in contract_changes() {
        change(root);
        places.push(place.to_owned());
    }
    bump(root, version("0.4.0")).expect("a bump");
    let mut changed = check(root).expect("admitted").changed;
    changed.sort();
    places.sort();
    assert_eq!(changed, places);
}

#[test]
fn a_release_no_newer_than_the_previous_is_refused() {
    let workspace = released();
    let refused = check(workspace.path());
    assert!(
        matches!(refused, Err(Error::NotNewer { .. })),
        "{refused:?}"
    );
}

#[test]
fn an_unknown_previous_release_is_refused() {
    let workspace = released();
    let root = workspace.path();
    bump(root, version("0.3.1")).expect("a bump");
    let refused = check_release(root, "v0.2.0", CONTRACTS_UNDER_TEST);
    assert!(matches!(refused, Err(Error::Git(_))), "{refused:?}");
}

#[test]
fn a_contract_that_names_nothing_is_refused() {
    let workspace = released();
    let root = workspace.path();
    for contract in [
        Contract::Records("absent.txt"),
        Contract::File("absent.json"),
        Contract::Constant {
            file: "crates/alpha/src/lib.rs",
            name: "ABSENT",
        },
        Contract::Constant {
            file: "crates/alpha/src/lib.rs",
            name: "FORMAT",
        },
    ] {
        assert!(resolve(root, contract).is_err(), "{contract:?}");
    }
}

#[test]
fn every_clerkenwell_contract_names_something() {
    for contract in CONTRACTS {
        let places = resolve(&workspace(), *contract)
            .unwrap_or_else(|error| panic!("{contract:?}: {error}"));
        assert!(!places.is_empty(), "{contract:?}");
    }
}
