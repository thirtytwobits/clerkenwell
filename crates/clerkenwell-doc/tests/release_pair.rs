//! The Rust and npm Loro releases the workspace builds against are the pair
//! `loro-release.json` approves.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn json(path: &str) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn toml(path: &str) -> toml::Table {
    read(path)
        .parse()
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn release() -> Value {
    json("loro-release.json")
}

fn git_source(release: &Value) -> (&str, &str) {
    let source = &release["source"];
    assert_eq!(source["kind"], "git", "this pair is a Git release");
    (
        source["url"].as_str().expect("source url"),
        source["revision"].as_str().expect("source revision"),
    )
}

#[test]
fn the_manifest_pins_loro_and_its_btree_override_to_the_approved_revision() {
    let release = release();
    let (url, revision) = git_source(&release);
    let manifest = toml("Cargo.toml");
    let pinned = |table: &toml::Value| {
        table.get("git").and_then(toml::Value::as_str) == Some(url)
            && table.get("rev").and_then(toml::Value::as_str) == Some(revision)
            && table.as_table().is_some_and(|fields| fields.len() == 2)
    };
    assert!(pinned(&manifest["workspace"]["dependencies"]["loro"]));
    assert!(pinned(&manifest["patch"]["crates-io"]["generic-btree"]));
}

#[test]
fn the_lockfile_resolves_exactly_the_approved_core_crates() {
    let release = release();
    let (url, revision) = git_source(&release);
    let expected_source = format!("git+{url}?rev={revision}#{revision}");
    let lock = toml("Cargo.lock");
    let packages = lock["package"].as_array().expect("lock packages");
    let crates = release["rust"]["crates"]
        .as_object()
        .expect("approved crates");
    for (name, version) in crates {
        let matching = packages
            .iter()
            .filter(|package| package["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "one {name} in Cargo.lock");
        assert_eq!(matching[0]["version"].as_str(), version.as_str(), "{name}");
        assert_eq!(
            matching[0]["source"].as_str(),
            Some(expected_source.as_str()),
            "{name}"
        );
    }
    let approved_registry = release["rust"]["registryCrates"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for package in packages {
        let name = package["name"].as_str().expect("package name");
        if name.starts_with("loro") {
            assert!(
                crates.contains_key(name) || approved_registry.contains_key(name),
                "{name} is not an approved Loro crate"
            );
        }
    }
}

#[test]
fn the_typescript_client_installs_the_approved_npm_package() {
    let release = release();
    let npm = &release["npm"];
    let manifest = json("clients/typescript/client/package.json");
    assert_eq!(manifest["dependencies"]["loro-crdt"], npm["url"]);
    let lock = json("package-lock.json");
    let installed = lock["packages"]
        .as_object()
        .expect("lock packages")
        .iter()
        .filter(|(path, _)| path.ends_with("node_modules/loro-crdt"))
        .collect::<Vec<_>>();
    assert_eq!(installed.len(), 1, "one loro-crdt in package-lock.json");
    let (_, package) = installed[0];
    assert_eq!(package["version"], npm["version"]);
    assert_eq!(package["resolved"], npm["url"]);
    assert_eq!(package["integrity"], npm["integrity"]);
}
