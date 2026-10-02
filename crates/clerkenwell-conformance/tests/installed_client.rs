//! A consumer's npm project drives conformance against the
//! `@clerkenwell/client` it installed from the packed package.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use clerkenwell_conformance::{workspace, Bindings, Conformance};
use clerkenwell_notebook::GENERATED_COLLABORATION_SPECS;
use serde_json::Value;

fn notebook() -> PathBuf {
    workspace().join("crates/clerkenwell-notebook")
}

fn npm(args: &[&str], directory: &Path) -> String {
    let output = Command::new("npm")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("npm runs");
    assert!(
        output.status.success(),
        "npm {args:?} in {}: {}",
        directory.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("npm prints text")
}

/// `@clerkenwell/client` packed as it is published.
fn packed_client() -> &'static Path {
    static PACKED: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &PACKED
        .get_or_init(|| {
            let destination = tempfile::tempdir().expect("a temporary directory");
            let packed = npm(
                &[
                    "pack",
                    "--json",
                    "--pack-destination",
                    destination.path().to_str().expect("a UTF-8 path"),
                ],
                &workspace().join("clients/typescript/client"),
            );
            let packed: Value = serde_json::from_str(&packed).expect("npm pack prints JSON");
            let tarball = destination.path().join(
                packed[0]["filename"]
                    .as_str()
                    .expect("npm pack names the tarball"),
            );
            (destination, tarball)
        })
        .1
}

/// An npm project with the packed client and `tsx` installed, and the
/// notebook's generated TypeScript plans in it.
fn consumer() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(
        project.path().join("package.json"),
        r#"{ "name": "consumer", "private": true, "type": "module" }"#,
    )
    .expect("writable");
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace().join("package.json")).expect("readable"),
    )
    .expect("JSON");
    let tsx = manifest["devDependencies"]["tsx"]
        .as_str()
        .expect("the workspace develops with tsx");
    npm(
        &[
            "install",
            "--no-audit",
            "--no-fund",
            "--prefer-offline",
            packed_client().to_str().expect("a UTF-8 path"),
            &format!("tsx@{tsx}"),
        ],
        project.path(),
    );
    std::fs::create_dir(project.path().join("plans")).expect("a directory");
    std::fs::copy(
        notebook().join("generated/typescript/index.ts"),
        project.path().join("plans/index.ts"),
    )
    .expect("a copy");
    project
}

fn bindings(project: &Path) -> Bindings {
    Bindings {
        plans: GENERATED_COLLABORATION_SPECS,
        typescript_plans: project.join("plans/index.ts"),
        collaboration_fixtures: notebook().join("generated/notebook.collaboration.fixtures.json"),
    }
}

#[test]
fn a_consumer_drives_conformance_against_the_client_it_installed() {
    let project = consumer();
    // The client runs as published: from its built modules alone.
    std::fs::remove_dir_all(project.path().join("node_modules/@clerkenwell/client/src"))
        .expect("the package ships its source");
    let conformance = Conformance::start_in(&bindings(project.path()), project.path());
    conformance.seeding();
    conformance.fixture_operations();
    conformance.concurrent_edits();
    conformance.concurrent_typing();
    conformance.text_consumption();
    conformance.conflict_policy();
}

#[test]
#[should_panic(expected = "has @clerkenwell/client 99.0.0")]
fn a_client_of_another_version_is_refused() {
    let project = consumer();
    let manifest = project
        .path()
        .join("node_modules/@clerkenwell/client/package.json");
    let text = std::fs::read_to_string(&manifest).expect("readable");
    let installed = format!("\"version\": \"{}\"", env!("CARGO_PKG_VERSION"));
    assert!(
        text.contains(&installed),
        "the client is at this crate's version"
    );
    std::fs::write(
        &manifest,
        text.replacen(&installed, "\"version\": \"99.0.0\"", 1),
    )
    .expect("writable");
    Conformance::start_in(&bindings(project.path()), project.path());
}

#[test]
#[should_panic(expected = "needs tsx installed")]
fn a_project_without_tsx_is_refused() {
    let project = tempfile::tempdir().expect("a temporary directory");
    Conformance::start_in(&bindings(project.path()), project.path());
}
