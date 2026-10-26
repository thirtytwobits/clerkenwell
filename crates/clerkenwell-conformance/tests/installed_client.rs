//! A consumer's npm project drives conformance against the
//! `@clerkenwell/client` it installed from the packed package.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::SystemTime;

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

/// `@clerkenwell/client` packed as it is published, once for this process.
fn packed_client() -> &'static Path {
    static PACKED: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &PACKED
        .get_or_init(|| {
            let destination = tempfile::tempdir().expect("a temporary directory");
            let tarball = pack_client(destination.path());
            (destination, tarball)
        })
        .1
}

/// The workspace's `@clerkenwell/client`.
fn client() -> PathBuf {
    workspace().join("clients/typescript/client")
}

/// Packs `@clerkenwell/client` into `destination` and returns the tarball.
/// The pack builds `dist` in a copy of the client, so it only reads the
/// workspace's client. The copy keeps the client's place relative to the
/// workspace's TypeScript configuration and lies inside the workspace's
/// `node_modules`, from which its build resolves the workspace's packages.
fn pack_client(destination: &Path) -> PathBuf {
    let cache = workspace().join("node_modules/.cache/clerkenwell-conformance");
    std::fs::create_dir_all(&cache).expect("a cache directory");
    let build = tempfile::tempdir_in(&cache).expect("a temporary directory");
    std::fs::copy(
        workspace().join("tsconfig.base.json"),
        build.path().join("tsconfig.base.json"),
    )
    .expect("a copy");
    let copy = build.path().join("clients/typescript/client");
    copy_sources(&client(), &copy);
    let packed = npm(
        &[
            "pack",
            "--json",
            "--pack-destination",
            destination.to_str().expect("a UTF-8 path"),
        ],
        &copy,
    );
    let packed: Value = serde_json::from_str(&packed).expect("npm pack prints JSON");
    destination.join(
        packed[0]["filename"]
            .as_str()
            .expect("npm pack names the tarball"),
    )
}

/// Copies the package at `from` to `to`, leaving out the `dist` it builds.
fn copy_sources(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a directory");
    for entry in std::fs::read_dir(from).expect("a readable directory") {
        let entry = entry.expect("a readable entry");
        if entry.file_name() == "dist" || entry.file_name() == "node_modules" {
            continue;
        }
        copy_tree(&entry.path(), &to.join(entry.file_name()));
    }
}

fn copy_tree(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir_all(to).expect("a directory");
        for entry in std::fs::read_dir(from).expect("a readable directory") {
            let entry = entry.expect("a readable entry");
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        std::fs::copy(from, to).expect("a copy");
    }
}

/// An npm project with the packed client and `tsx` installed, and the
/// notebook's generated TypeScript plans in it.
fn consumer() -> tempfile::TempDir {
    consumer_in(&std::env::temp_dir())
}

/// A consumer project in a new directory under `parent`.
fn consumer_in(parent: &Path) -> tempfile::TempDir {
    let project = tempfile::tempdir_in(parent).expect("a temporary directory");
    std::fs::write(
        project.path().join("package.json"),
        r#"{ "name": "consumer", "private": true, "type": "module" }"#,
    )
    .expect("writable");
    npm(
        &[
            "install",
            "--no-audit",
            "--no-fund",
            "--prefer-offline",
            packed_client().to_str().expect("a UTF-8 path"),
            &developed_with("tsx"),
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

/// `package` at the version this workspace develops with.
fn developed_with(package: &str) -> String {
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace().join("package.json")).expect("readable"),
    )
    .expect("JSON");
    let version = manifest["devDependencies"][package]
        .as_str()
        .unwrap_or_else(|| panic!("the workspace develops with {package}"));
    format!("{package}@{version}")
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

/// A consumer project with `typescript` installed, whose client runs from its
/// built modules and declarations alone.
fn consumer_with(typescript: &str) -> tempfile::TempDir {
    let project = consumer();
    npm(
        &[
            "install",
            "--no-audit",
            "--no-fund",
            "--prefer-offline",
            typescript,
        ],
        project.path(),
    );
    std::fs::remove_dir_all(project.path().join("node_modules/@clerkenwell/client/src"))
        .expect("the package ships its source");
    project
}

/// Adds a declaration to the consumer's plans module that is not a value of
/// its type.
fn break_plans(project: &Path) {
    let plans = project.join("plans/index.ts");
    let mut source = std::fs::read_to_string(&plans).expect("readable");
    source.push_str("\nexport const notANumber: number = \"text\";\n");
    std::fs::write(&plans, source).expect("writable");
}

#[test]
fn a_consumer_checks_documents_against_the_declarations_of_the_client_it_installed() {
    let project = consumer_with(&developed_with("typescript"));
    Conformance::start_in(&bindings(project.path()), project.path()).document_types();
}

#[test]
fn a_consumer_on_typescript_7_checks_documents_against_the_declarations_of_the_client_it_installed()
{
    let project = consumer_with("typescript@7");
    Conformance::start_in(&bindings(project.path()), project.path()).document_types();
}

#[test]
#[should_panic(expected = "is not a value of its CollaborationDocument")]
fn a_type_error_in_the_plans_fails_the_document_check() {
    let project = consumer_with(&developed_with("typescript"));
    break_plans(project.path());
    Conformance::start_in(&bindings(project.path()), project.path()).document_types();
}

#[test]
#[should_panic(expected = "is not a value of its CollaborationDocument")]
fn a_type_error_in_the_plans_fails_the_document_check_on_typescript_7() {
    let project = consumer_with("typescript@7");
    break_plans(project.path());
    Conformance::start_in(&bindings(project.path()), project.path()).document_types();
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
fn a_project_whose_packages_a_workspace_above_it_installed_drives_conformance() {
    let workspace = consumer();
    let project = workspace.path().join("app");
    std::fs::create_dir_all(project.join("plans")).expect("a directory");
    std::fs::rename(
        workspace.path().join("plans/index.ts"),
        project.join("plans/index.ts"),
    )
    .expect("a move");
    assert!(!project.join("node_modules/@clerkenwell/client").exists());
    Conformance::start_in(&bindings(&project), &project).seeding();
}

/// `path` relative to this process's working directory, through their
/// nearest common ancestor.
fn relative(path: &Path) -> PathBuf {
    let here = std::env::current_dir()
        .and_then(|here| here.canonicalize())
        .expect("a working directory");
    let path = path.canonicalize().expect("an existing path");
    let shared = here
        .components()
        .zip(path.components())
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative: PathBuf = here
        .components()
        .skip(shared)
        .map(|_| Component::ParentDir)
        .collect();
    relative.extend(path.components().skip(shared));
    relative
}

#[test]
fn a_project_given_by_a_relative_path_drives_conformance() {
    // Under the build's own directory, so the path to it does not climb to
    // the filesystem root, which every directory reaches alike.
    let project = consumer_in(Path::new(env!("CARGO_TARGET_TMPDIR")));
    let project = relative(project.path());
    assert!(project.starts_with(".."), "{}", project.display());
    let bindings = bindings(&project);
    assert!(bindings.typescript_plans.is_relative());
    Conformance::start_in(&bindings, &project).seeding();
}

#[test]
#[should_panic(expected = "needs tsx installed")]
fn a_project_without_tsx_is_refused() {
    let project = consumer();
    std::fs::remove_dir_all(project.path().join("node_modules/tsx")).expect("removable");
    Conformance::start_in(&bindings(project.path()), project.path());
}

#[test]
#[should_panic(expected = "needs @clerkenwell/client installed")]
fn a_project_without_the_client_is_refused() {
    let project = consumer();
    std::fs::remove_dir_all(project.path().join("node_modules/@clerkenwell/client"))
        .expect("removable");
    Conformance::start_in(&bindings(project.path()), project.path());
}

/// Every file under `directory`, with when it was last modified.
fn files(directory: &Path) -> BTreeMap<PathBuf, SystemTime> {
    let mut files = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("a readable directory") {
            let entry = entry.expect("a readable entry");
            let metadata = entry.metadata().expect("readable metadata");
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                files.insert(
                    entry.path(),
                    metadata.modified().expect("a modification time"),
                );
            }
        }
    }
    files
}

/// A pack only reads the workspace's client, so packs in concurrent test
/// processes cannot read a file another one is writing. A rewrite with the
/// same contents is a write, so the test compares modification times, not
/// contents.
#[test]
fn packing_the_client_leaves_the_workspace_client_unchanged() {
    let before = files(&client());
    let destination = tempfile::tempdir().expect("a temporary directory");
    assert!(pack_client(destination.path()).is_file());
    assert_eq!(files(&client()), before);
}
