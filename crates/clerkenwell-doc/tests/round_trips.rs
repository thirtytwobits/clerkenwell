//! A replica stores a document in the container layout its plan declares and
//! reads it back unchanged, and a rewrite writes only what it changes.

mod support;

use std::collections::HashMap;

use clerkenwell_doc::loro::{ContainerID, ToJson};
use clerkenwell_doc::{CollaborationLoroError, LoroAuthoringDocument};
use clerkenwell_notebook::{
    GeneratedCollaborationEntitySpec, GeneratedCollaborationStorageKind,
    GeneratedCollaborationValueCodec, GENERATED_COLLABORATION_SPECS,
};
use serde_json::{json, Value};
use support::*;

#[test]
fn every_fixture_document_reads_back_unchanged_from_its_replica_and_its_history() {
    for plan in GENERATED_COLLABORATION_SPECS {
        let document = wire_document(plan);
        let replica = seed(plan, &document);
        let expected = at_revision(plan, &document, REVISION);
        assert_eq!(read(&replica), expected, "{} replica", plan.name);
        assert_eq!(reread(plan, &replica), expected, "{} history", plan.name);
    }
}

#[test]
fn a_materialisation_carries_the_revision_it_was_read_at() {
    for plan in GENERATED_COLLABORATION_SPECS {
        let replica = seed(plan, &wire_document(plan));
        for revision in ["loro:first", "loro:second"] {
            assert_eq!(
                replica
                    .materialized_document(revision)
                    .expect("materialise"),
                at_revision(plan, &wire_document(plan), revision),
                "{}",
                plan.name
            );
        }
    }
}

fn resolve(template: &str, identities: &HashMap<String, String>) -> String {
    identities
        .iter()
        .fold(template.to_string(), |resolved, (variable, identity)| {
            resolved.replace(&format!("{{{variable}}}"), identity)
        })
}

/// Checks every field the plan lays out directly — scalars, text, ordered
/// lists and keyed sequence order — at the depth of `prefix`.
fn assert_layout(
    plan: &GeneratedCollaborationEntitySpec,
    doc: &clerkenwell_doc::loro::LoroDoc,
    prefix: &str,
    node: &Value,
    identities: &HashMap<String, String>,
) {
    for field in plan.fields {
        let Some(relative) = field.path.strip_prefix(prefix) else {
            continue;
        };
        if relative.contains('*') {
            continue;
        }
        let Some(value) = node.pointer(&pointer(relative)) else {
            continue;
        };
        let container = field
            .container
            .or(field.container_template)
            .map(|template| resolve(template, identities));
        let context = format!("{}.{} at {identities:?}", plan.name, field.path);
        match field.storage_kind {
            GeneratedCollaborationStorageKind::Scalar => {
                let stored = doc
                    .get_map(container.expect("a scalar container").as_str())
                    .get(field.key.expect("a scalar key"))
                    .unwrap_or_else(|| panic!("{context} is stored"))
                    .get_deep_value()
                    .to_json_value();
                assert_eq!(&stored, value, "{context}");
            }
            GeneratedCollaborationStorageKind::Text => {
                let text = doc
                    .get_text(container.expect("a text container").as_str())
                    .to_string();
                if field.codec == GeneratedCollaborationValueCodec::PropertyText {
                    assert_eq!(json!(text), value["value"], "{context}");
                    let metadata = field
                        .metadata_container
                        .or(field.metadata_container_template)
                        .map(|template| resolve(template, identities))
                        .expect("a MIME container");
                    let mime = doc
                        .get_map(metadata.as_str())
                        .get(field.metadata_key.expect("a MIME key"))
                        .unwrap_or_else(|| panic!("{context} stores its MIME type"))
                        .get_deep_value()
                        .to_json_value();
                    assert_eq!(mime, value["$mime"], "{context}");
                } else {
                    assert_eq!(json!(text), *value, "{context}");
                }
            }
            GeneratedCollaborationStorageKind::OrderedList => {
                let list = doc
                    .get_list(container.expect("a list container").as_str())
                    .get_deep_value()
                    .to_json_value();
                assert_eq!(&list, value, "{context}");
            }
            GeneratedCollaborationStorageKind::KeyedSequence => {
                let identity_path = field.identity_path.expect("an identity path");
                let variable = field.identity_variable.unwrap_or(identity_path);
                let order = doc
                    .get_list(
                        resolve(
                            field.order_container.expect("an order container"),
                            identities,
                        )
                        .as_str(),
                    )
                    .get_deep_value()
                    .to_json_value();
                let items = value.as_array().expect("sequence items");
                let expected = items
                    .iter()
                    .map(|item| item[identity_path].clone())
                    .collect::<Vec<_>>();
                assert_eq!(order, json!(expected), "{context}");
                for item in items {
                    let mut nested = identities.clone();
                    nested.insert(
                        variable.to_string(),
                        item[identity_path]
                            .as_str()
                            .expect("an identity")
                            .to_string(),
                    );
                    assert_layout(plan, doc, &format!("{}.*.", field.path), item, &nested);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn a_seeded_document_is_stored_in_the_containers_its_plan_declares() {
    for (plan, document) in GENERATED_COLLABORATION_SPECS
        .iter()
        .map(|plan| (plan, wire_document(plan)))
        .chain([(BOARD, board())])
    {
        let replica = seed(plan, &document);
        assert_layout(plan, &raw(&replica), "", &document, &HashMap::new());
    }
}

#[test]
fn a_rewrite_writes_operations_only_to_the_containers_of_the_fields_it_changes() {
    let base = wire_document(NOTE);
    let accepted = seed(NOTE, &base).export_update_base64().expect("export");
    let mut writer = hydrate(NOTE, &accepted);
    let before = raw(&writer).oplog_vv();
    let edited = with(&base, "/title", json!("A retitled note"));
    writer.replace_document(&edited).expect("rewrite");

    let doc = raw(&writer);
    let written = doc.export_json_updates(&before, &doc.oplog_vv());
    let title = field(NOTE, "title");
    let containers = written
        .changes
        .iter()
        .flat_map(|change| change.ops.iter().map(|op| op.container.clone()))
        .collect::<Vec<_>>();
    assert!(!containers.is_empty(), "the rewrite wrote the title");
    for container in containers {
        assert!(
            matches!(&container, ContainerID::Root { name, .. } if name.as_str() == title.container.expect("title container")),
            "{container:?} is not the title's container"
        );
    }
    assert_eq!(reread(NOTE, &writer), at_revision(NOTE, &edited, REVISION));
}

#[test]
fn rewriting_a_document_unchanged_writes_nothing() {
    for (plan, document) in GENERATED_COLLABORATION_SPECS
        .iter()
        .map(|plan| (plan, wire_document(plan)))
        .chain([(BOARD, board())])
    {
        let mut replica = seed(plan, &document);
        let frontier = replica.accepted_frontier_base64();
        replica.replace_document(&document).expect("rewrite");
        assert_eq!(
            replica.accepted_frontier_base64(),
            frontier,
            "{}",
            plan.name
        );
    }
}

#[test]
fn property_text_keeps_its_mime_type_through_a_round_trip() {
    let plain = json!({ "$mime": "text/plain", "value": "Plain prose." });
    let custom = json!({ "$mime": "text/x-notebook", "value": "Custom prose." });
    let note = with(&wire_document(NOTE), "/body", plain);
    let mut board = board();
    set(&mut board, "/columns/1/cards/0/note", custom);
    for (plan, document) in [(NOTE, note), (BOARD, board)] {
        let replica = seed(plan, &document);
        assert_eq!(
            reread(plan, &replica),
            at_revision(plan, &document, REVISION),
            "{}",
            plan.name
        );
    }
}

#[test]
fn a_replica_that_adopts_an_accepted_update_holds_the_accepted_state_and_history() {
    for plan in GENERATED_COLLABORATION_SPECS {
        let base = wire_document(plan);
        let accepted = seed(plan, &base).export_update_base64().expect("export");
        let mut client = hydrate(plan, &accepted);
        let mut server = hydrate(plan, &accepted);
        let scalar = &fixture(plan.name)["operations"]["scalar"];
        let edited = with(
            &base,
            &pointer(scalar["path"].as_str().expect("scalar path")),
            scalar["value"].clone(),
        );
        server.replace_document(&edited).expect("server edit");

        client
            .adopt_versioned_update_base64(
                plan.schema_version,
                &server.export_update_base64().expect("export"),
            )
            .expect("adopt");

        assert_eq!(read(&client), read(&server), "{}", plan.name);
        assert_eq!(
            client.accepted_frontier_base64(),
            server.accepted_frontier_base64(),
            "{} adopted no operations of its own",
            plan.name
        );
        // The adopted document is what the next rewrite is diffed against.
        client.replace_document(&edited).expect("rewrite adopted");
        assert_eq!(
            client.accepted_frontier_base64(),
            server.accepted_frontier_base64(),
            "{}",
            plan.name
        );
    }
}

#[test]
fn a_document_missing_a_required_field_is_refused_naming_the_field() {
    for plan in GENERATED_COLLABORATION_SPECS {
        let document = wire_document(plan);
        for field in plan.fields.iter().filter(|field| {
            field.required
                && !field.path.contains('*')
                && !matches!(
                    field.storage_kind,
                    GeneratedCollaborationStorageKind::DerivedIdentity
                        | GeneratedCollaborationStorageKind::DerivedRevision
                )
        }) {
            let mut missing = document.clone();
            remove(&mut missing, &pointer(field.path));
            match LoroAuthoringDocument::from_document(plan, &missing) {
                Err(CollaborationLoroError::InvalidField { path, .. }) => {
                    assert_eq!(path, field.path, "{}", plan.name)
                }
                other => panic!("{}.{} missing: {other:?}", plan.name, field.path),
            }
        }
    }
}

#[test]
fn a_keyed_item_missing_a_required_field_is_refused_and_changes_nothing() {
    let base = board();
    let mut replica = seed(BOARD, &base);
    let before = (replica.accepted_frontier_base64(), read(&replica));
    for pointer in [
        "/columns/0/title",
        "/columns/0/cards/0/text",
        "/columns/1/cards/0/note",
    ] {
        let mut missing = base.clone();
        remove(&mut missing, pointer);
        assert!(
            LoroAuthoringDocument::from_document(BOARD, &missing).is_err(),
            "{pointer}"
        );
        assert!(replica.replace_document(&missing).is_err(), "{pointer}");
        assert_eq!(
            (replica.accepted_frontier_base64(), read(&replica)),
            before,
            "{pointer}"
        );
    }
}

#[test]
fn empty_keyed_sequences_read_back_empty() {
    let mut empty_column = board();
    items_mut(&mut empty_column, "/columns/1/cards").clear();
    let mut no_columns = board();
    items_mut(&mut no_columns, "/columns").clear();
    for document in [empty_column, no_columns] {
        let replica = seed(BOARD, &document);
        assert_eq!(
            reread(BOARD, &replica),
            at_revision(BOARD, &document, REVISION)
        );
    }
}

#[test]
fn columns_holding_different_cards_read_back_with_their_own_cards() {
    let document = board();
    let replica = seed(BOARD, &document);
    assert_eq!(
        reread(BOARD, &replica),
        at_revision(BOARD, &document, REVISION)
    );
}
