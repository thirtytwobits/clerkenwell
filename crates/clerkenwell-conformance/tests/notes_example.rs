//! Rust and TypeScript replicas of the notes example's entity converge.

use std::path::{Path, PathBuf};

use clerkenwell_conformance::{check_client_naming, Bindings, Conformance};
use clerkenwell_example_notes::model::GENERATED_COLLABORATION_SPECS;

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/notes")
}

fn bindings() -> Bindings {
    Bindings {
        plans: GENERATED_COLLABORATION_SPECS,
        typescript_plans: example().join("generated/typescript/index.ts"),
        collaboration_fixtures: example().join("generated/notes.collaboration.fixtures.json"),
    }
}

#[test]
fn rust_and_typescript_replicas_of_the_notes_example_converge() {
    check_client_naming(&bindings());
    let conformance = Conformance::start(&bindings());
    conformance.seeding();
    conformance.fixture_operations();
    conformance.concurrent_edits();
    conformance.concurrent_typing();
    conformance.text_consumption();
}
