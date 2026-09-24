//! The operations and invalid cases the generator emits for every
//! collaborative entity: each operation reads back as written, and each
//! invalid case is refused without changing the replica.

mod support;

use clerkenwell_doc::{CollaborationLoroError, LoroAuthoringDocument};
use clerkenwell_notebook::{
    GeneratedCollaborationEntitySpec, GeneratedCollaborationStorageKind,
    GENERATED_COLLABORATION_SPECS,
};
use serde_json::Value;
use support::*;

fn operations(plan: &GeneratedCollaborationEntitySpec) -> Value {
    fixture(plan.name)["operations"].clone()
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("a list")
        .iter()
        .map(|entry| entry.as_str().expect("a path").to_string())
        .collect()
}

/// Whether an optional path names a member of an object inside the document
/// or its keyed item, rather than a field of the document or item itself.
fn is_group_member(path: &str) -> bool {
    path.rsplit(".*.")
        .next()
        .is_some_and(|relative| relative.contains('.'))
}

type Edit = Box<dyn Fn(&Value) -> Value>;

/// Every edit the fixture's operations describe, in order, each applied to
/// the document the previous one left. `optional` selects the optional paths
/// the presence operations cover.
fn fixture_edits(
    plan: &GeneratedCollaborationEntitySpec,
    optional: impl Fn(&str) -> bool,
) -> Vec<(String, Edit)> {
    let operations = operations(plan);
    let fixture = wire_document(plan);
    let mut edits: Vec<(String, Edit)> = Vec::new();

    let scalar = operations["scalar"].clone();
    let path = scalar["path"].as_str().expect("scalar path").to_string();
    edits.push((
        format!("scalar {path}"),
        Box::new(move |document| {
            let mut edited = document.clone();
            for at in pointers(document, &path) {
                set(&mut edited, &at, scalar["value"].clone());
            }
            edited
        }),
    ));

    let absent = string_list(&operations["optionalAbsent"])
        .into_iter()
        .filter(|path| optional(path))
        .collect::<Vec<_>>();
    edits.push((
        format!("optionalAbsent {absent:?}"),
        Box::new(move |document| {
            let mut edited = document.clone();
            for path in &absent {
                for at in pointers(document, path) {
                    remove(&mut edited, &at);
                }
            }
            edited
        }),
    ));

    let present = string_list(&operations["optionalPresent"])
        .into_iter()
        .filter(|path| optional(path))
        .collect::<Vec<_>>();
    edits.push((
        format!("optionalPresent {present:?}"),
        Box::new(move |document| {
            let mut edited = document.clone();
            for path in &present {
                for at in pointers(&fixture, path) {
                    set(
                        &mut edited,
                        &at,
                        fixture.pointer(&at).expect("fixture value").clone(),
                    );
                }
            }
            edited
        }),
    ));

    for reorder in operations["reorder"].as_array().expect("reorders").clone() {
        let path = reorder["path"].as_str().expect("reorder path").to_string();
        edits.push((
            format!("reorder {path}"),
            Box::new(move |document| {
                let mut edited = document.clone();
                for at in pointers(document, &path) {
                    let current = items(document, &at);
                    let order = reorder["order"].as_array().expect("order");
                    *items_mut(&mut edited, &at) = order
                        .iter()
                        .map(|index| current[index.as_u64().expect("an index") as usize].clone())
                        .collect();
                }
                edited
            }),
        ));
    }

    for delete in operations["delete"].as_array().expect("deletes").clone() {
        let path = delete["path"].as_str().expect("delete path").to_string();
        edits.push((
            format!("delete {path}"),
            Box::new(move |document| {
                let mut edited = document.clone();
                for at in pointers(document, &path) {
                    items_mut(&mut edited, &at)
                        .remove(delete["index"].as_u64().expect("an index") as usize);
                }
                edited
            }),
        ));
    }
    edits
}

#[test]
fn every_fixture_operation_reads_back_as_written() {
    for plan in GENERATED_COLLABORATION_SPECS {
        let mut document = wire_document(plan);
        let mut replica = seed(plan, &document);
        for (name, edit) in fixture_edits(plan, |path| !is_group_member(path)) {
            let edited = edit(&document);
            assert_ne!(
                edited, document,
                "{} {name} changes the document",
                plan.name
            );
            replica.replace_document(&edited).expect("apply operation");
            let expected = at_revision(plan, &edited, REVISION);
            assert_eq!(read(&replica), expected, "{} {name}", plan.name);
            assert_eq!(
                reread(plan, &replica),
                expected,
                "{} {name} history",
                plan.name
            );
            document = edited;
        }
    }
}

#[test]
#[ignore = "defect: both replicas complete a present group with the empty value of each absent optional member"]
fn an_absent_optional_member_of_a_present_group_reads_back_absent() {
    for plan in GENERATED_COLLABORATION_SPECS {
        let document = wire_document(plan);
        for path in string_list(&operations(plan)["optionalAbsent"])
            .into_iter()
            .filter(|path| is_group_member(path))
        {
            let mut absent = document.clone();
            for at in pointers(&document, &path) {
                remove(&mut absent, &at);
            }
            let mut replica = seed(plan, &document);
            replica.replace_document(&absent).expect("remove");
            assert_eq!(
                read(&replica),
                at_revision(plan, &absent, REVISION),
                "{}.{path}",
                plan.name
            );
            assert_eq!(
                read(&seed(plan, &absent)),
                at_revision(plan, &absent, REVISION),
                "{}.{path}",
                plan.name
            );
        }
    }
}

#[test]
fn fixture_operations_from_two_replicas_converge_in_both_import_orders() {
    for plan in GENERATED_COLLABORATION_SPECS {
        let base = wire_document(plan);
        let edits = fixture_edits(plan, |_| true);
        for (one_name, one) in &edits {
            for (two_name, two) in &edits {
                if one_name == two_name {
                    continue;
                }
                let (forward, reverse) = merge_rewrites(plan, &base, &[one(&base), two(&base)]);
                assert_eq!(
                    forward, reverse,
                    "{}: {one_name} with {two_name}",
                    plan.name
                );
            }
        }
    }
}

fn keyed_sequences(plan: &GeneratedCollaborationEntitySpec) -> Vec<&'static str> {
    plan.fields
        .iter()
        .filter(|field| field.storage_kind == GeneratedCollaborationStorageKind::KeyedSequence)
        .map(|field| field.path)
        .collect()
}

/// The fixture document broken as an identity case describes, at the first
/// items of `sequence`.
fn broken(plan: &'static GeneratedCollaborationEntitySpec, kind: &str, sequence: &str) -> Value {
    let identity_path = field(plan, sequence)
        .identity_path
        .expect("an identity path");
    let mut document = wire_document(plan);
    let at = &pointers(&document, sequence)[0];
    match kind {
        "missingIdentity" => remove(&mut document, &format!("{at}/0/{identity_path}")),
        "duplicateIdentity" => {
            let first = document
                .pointer(&format!("{at}/0/{identity_path}"))
                .expect("first identity")
                .clone();
            set(&mut document, &format!("{at}/1/{identity_path}"), first);
        }
        other => panic!("{other} is not an identity case"),
    }
    document
}

fn observed(replica: &LoroAuthoringDocument) -> (String, Value) {
    (replica.accepted_frontier_base64(), read(replica))
}

#[test]
fn every_fixture_invalid_case_is_refused_without_changing_the_replica() {
    for plan in GENERATED_COLLABORATION_SPECS {
        let sequences = keyed_sequences(plan);
        let mut replica = seed(plan, &wire_document(plan));
        let update = replica.export_update_base64().expect("export");
        let before = observed(&replica);
        for case in fixture(plan.name)["invalid"]
            .as_array()
            .expect("invalid cases")
        {
            let kind = case["kind"].as_str().expect("case kind");
            match kind {
                "unsupportedSchemaVersion" => {
                    let version = case["schemaVersion"].as_u64().expect("version") as u32;
                    assert_ne!(version, plan.schema_version);
                    let unsupported = |result: Result<(), CollaborationLoroError>| {
                        assert!(
                            matches!(
                                result,
                                Err(CollaborationLoroError::UnsupportedSchemaVersion { .. })
                            ),
                            "{} version {version}",
                            plan.name
                        )
                    };
                    unsupported(
                        LoroAuthoringDocument::from_versioned_update_base64(plan, version, &update)
                            .map(|_| ()),
                    );
                    unsupported(replica.import_versioned_update_base64(version, &update));
                    unsupported(replica.adopt_versioned_update_base64(version, &update));
                }
                "missingIdentity" | "duplicateIdentity" => {
                    let Some(sequence) = case["sequencePath"].as_str() else {
                        assert!(
                            sequences.is_empty(),
                            "{} {kind} names none of its keyed sequences",
                            plan.name
                        );
                        continue;
                    };
                    let document = broken(plan, kind, sequence);
                    assert!(
                        LoroAuthoringDocument::from_document(plan, &document).is_err(),
                        "{} {kind} seeds",
                        plan.name
                    );
                    assert!(
                        replica.replace_document(&document).is_err(),
                        "{} {kind} rewrites",
                        plan.name
                    );
                }
                other => panic!("{} has an unknown invalid case {other}", plan.name),
            }
            assert_eq!(observed(&replica), before, "{} {kind}", plan.name);
        }
    }
}
