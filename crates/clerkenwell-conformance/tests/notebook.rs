//! Rust and TypeScript replicas of every notebook entity converge.

use std::path::{Path, PathBuf};

use clerkenwell_conformance::{check_client_naming, Bindings, Conformance};
use clerkenwell_notebook::GENERATED_COLLABORATION_SPECS;

fn notebook() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../clerkenwell-notebook")
}

fn bindings() -> Bindings {
    Bindings {
        plans: GENERATED_COLLABORATION_SPECS,
        typescript_plans: notebook().join("generated/typescript/index.ts"),
        collaboration_fixtures: notebook().join("generated/notebook.collaboration.fixtures.json"),
    }
}

#[test]
fn every_fixture_client_document_is_its_wire_document_in_client_naming() {
    check_client_naming(&bindings());
}

#[test]
fn each_language_hydrates_what_the_other_seeded() {
    Conformance::start(&bindings()).seeding();
}

#[test]
fn fixture_operations_written_in_either_language_read_back_in_the_other() {
    Conformance::start(&bindings()).fixture_operations();
}

#[test]
fn concurrent_fixture_operations_from_both_languages_converge() {
    Conformance::start(&bindings()).concurrent_edits();
}

#[test]
fn concurrent_typing_in_both_languages_keeps_both_insertions() {
    Conformance::start(&bindings()).concurrent_typing();
}

#[test]
fn typing_survives_the_consumption_of_captured_text() {
    Conformance::start(&bindings()).text_consumption();
}
