//! The recorder lists what a crate's public paths reach, and only that.

use std::path::Path;

use clerkenwell_public_api::{record, Record};

fn sample() -> Record {
    record(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample"))
        .expect("the sample crate is readable")
}

fn mentions(record: &Record, fragment: &str) -> bool {
    record.lines.iter().any(|line| line.contains(fragment))
}

#[test]
fn an_item_re_exported_from_a_private_module_is_recorded_at_its_public_path() {
    let record = sample();

    assert!(mentions(&record, "sample_crate::Reexported"));
    assert!(!mentions(&record, "sample_crate::hidden::"));
}

#[test]
fn an_item_in_a_public_module_is_recorded_under_it() {
    assert!(mentions(&sample(), "sample_crate::open::in_open"));
}

#[test]
fn nothing_outside_the_public_paths_is_recorded() {
    let record = sample();

    for unreachable in ["Unreachable", "crate_only", "private_method", "kept"] {
        assert!(!mentions(&record, unreachable), "{unreachable}");
    }
}

#[test]
fn an_item_that_exists_only_in_tests_is_not_recorded() {
    assert!(!mentions(&sample(), "only_in_tests"));
}

#[test]
fn an_item_behind_a_cfg_carries_its_condition() {
    let record = sample();
    let line = record
        .lines
        .iter()
        .find(|line| line.contains("behind_a_feature"))
        .expect("the gated item");

    assert!(line.starts_with("#[cfg(feature = \"extra\")]"), "{line}");
}

#[test]
fn a_public_type_is_recorded_with_its_public_fields_methods_and_trait_implementations() {
    let record = sample();

    assert!(mentions(&record, "sample_crate::Visible::field"));
    assert!(mentions(&record, "sample_crate::Visible::method"));
    assert!(mentions(&record, "for sample_crate::Visible"));
}

#[test]
fn the_crates_public_signatures_name_types_from_are_listed() {
    let record = sample();

    assert!(record.names.contains("serde_json"), "{:?}", record.names);
    assert!(
        record.names.contains("external_crate"),
        "{:?}",
        record.names
    );
    assert!(record.names.contains("whole_crate"), "{:?}", record.names);
    assert!(!record.names.contains("std"), "{:?}", record.names);
}

#[test]
fn a_record_names_the_crates_it_lists() {
    let record = sample();
    let text = record.text();

    for name in &record.names {
        assert!(text
            .lines()
            .take(2)
            .any(|line| line.contains(name.as_str())));
    }
    for line in &record.lines {
        assert!(text.lines().any(|written| written == line));
    }
}
