//! Field conflict policy: `immutable` fields may not change, `explicit` fields
//! refuse two different changes to one value, and `merge` and
//! `lastWriterWins` fields accept concurrent changes.

mod support;

use clerkenwell_doc::conflicting_field_paths;
use clerkenwell_notebook::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationStorageKind, GeneratedCollaborationValueCodec,
    GENERATED_COLLABORATION_SPECS,
};
use serde_json::{json, Value};
use support::*;

/// Two values for a field, each different from `original` and from each other.
fn two_edits(codec: GeneratedCollaborationValueCodec, original: &Value) -> Option<(Value, Value)> {
    use GeneratedCollaborationValueCodec as Codec;
    match codec {
        Codec::String | Codec::OptionalString => {
            let original = original.as_str()?;
            Some((
                json!(format!("{original} (one)")),
                json!(format!("{original} (two)")),
            ))
        }
        Codec::Integer => {
            let original = original.as_i64()?;
            Some((json!(original + 1), json!(original + 2)))
        }
        Codec::Number | Codec::OptionalNumber => {
            let original = original.as_f64()?;
            Some((json!(original + 1.0), json!(original + 2.0)))
        }
        Codec::PropertyText => {
            let text = original["value"].as_str()?;
            let edit = |suffix: &str| json!({ "$mime": original["$mime"], "value": format!("{text} ({suffix})") });
            Some((edit("one"), edit("two")))
        }
        Codec::StringList => {
            let mut one = original.as_array()?.clone();
            let mut two = one.clone();
            one.push(json!("one"));
            two.push(json!("two"));
            Some((json!(one), json!(two)))
        }
        _ => None,
    }
}

/// Every declared field outside a keyed sequence whose fixture value can be
/// edited two ways.
fn editable_fields(
    plan: &'static GeneratedCollaborationEntitySpec,
) -> Vec<(&'static str, GeneratedCollaborationConflict, Value, Value)> {
    let document = wire_document(plan);
    plan.fields
        .iter()
        .filter(|field| {
            !field.path.contains('*')
                && !matches!(
                    field.storage_kind,
                    GeneratedCollaborationStorageKind::DerivedIdentity
                        | GeneratedCollaborationStorageKind::DerivedRevision
                )
        })
        .filter_map(|field| {
            let original = document.pointer(&pointer(field.path))?;
            let (one, two) = two_edits(field.codec, original)?;
            Some((field.path, field.conflict, one, two))
        })
        .collect()
}

#[test]
fn every_plan_judges_its_fields_by_their_declared_policy() {
    let mut judged = Vec::new();
    for plan in GENERATED_COLLABORATION_SPECS {
        let base = wire_document(plan);
        for (path, conflict, one, two) in editable_fields(plan) {
            judged.push(conflict);
            let at = pointer(path);
            let client = with(&base, &at, one.clone());
            let diverged = with(&base, &at, two);
            let agreed = with(&base, &at, one);
            let reported = |current: &Value| {
                conflicting_field_paths(plan, &base, &client, current).contains(&path.to_string())
            };
            match conflict {
                GeneratedCollaborationConflict::Immutable => {
                    assert!(reported(&base), "{}.{path} may not change", plan.name);
                }
                GeneratedCollaborationConflict::Explicit => {
                    assert!(
                        reported(&diverged),
                        "{}.{path} refuses two different changes",
                        plan.name
                    );
                    assert!(
                        !reported(&agreed),
                        "{}.{path} accepts the same change twice",
                        plan.name
                    );
                    assert!(
                        !reported(&base),
                        "{}.{path} accepts a change nobody else made",
                        plan.name
                    );
                }
                GeneratedCollaborationConflict::Merge
                | GeneratedCollaborationConflict::LastWriterWins => {
                    assert!(
                        !reported(&diverged),
                        "{}.{path} accepts concurrent changes",
                        plan.name
                    );
                }
            }
        }
    }
    for policy in [
        GeneratedCollaborationConflict::Immutable,
        GeneratedCollaborationConflict::Explicit,
        GeneratedCollaborationConflict::Merge,
        GeneratedCollaborationConflict::LastWriterWins,
    ] {
        assert!(
            judged.contains(&policy),
            "the fixtures must exercise a {policy:?} field"
        );
    }
}

#[test]
fn a_keyed_item_field_is_judged_per_item_and_named_by_its_identity() {
    let base = board();
    let identity = base["columns"][0]["column_id"]
        .as_str()
        .expect("column identity")
        .to_string();
    let client = with(&base, "/columns/0/title", json!("Client title"));
    let current = with(&base, "/columns/0/title", json!("Current title"));
    let sibling_changed = with(&base, "/columns/1/title", json!("Sibling title"));

    assert_eq!(
        conflicting_field_paths(BOARD, &base, &client, &current),
        vec![format!("columns[{identity}].title")]
    );
    assert!(conflicting_field_paths(BOARD, &base, &client, &sibling_changed).is_empty());
}

#[test]
fn a_replica_refuses_an_update_that_diverges_from_what_it_has_accepted() {
    let seed_document = wire_document(NOTE);
    let server = seed(NOTE, &seed_document);
    let base_frontier = server.accepted_frontier_base64();
    let seeded = server.export_update_base64().expect("export seed");
    let edit = |at: &str, value: Value| {
        let mut replica = hydrate(NOTE, &seeded);
        replica
            .replace_document(&with(&seed_document, at, value))
            .expect("edit client");
        replica
            .export_incremental_update_base64(&base_frontier)
            .expect("export edit")
    };

    server
        .import_versioned_update_base64(
            NOTE.schema_version,
            &edit("/title", json!("Accepted title")),
        )
        .expect("accept the first edit");

    let judge = |update: &str| {
        server
            .policy_conflicts_for_incremental_update(&base_frontier, NOTE.schema_version, update)
            .expect("judge")
    };
    assert_eq!(
        judge(&edit("/title", json!("Rival title"))),
        vec!["title".to_string()]
    );
    assert!(judge(&edit("/title", json!("Accepted title"))).is_empty());
    assert!(judge(&edit("/summary", json!("Merged prose."))).is_empty());
    assert_eq!(
        judge(&edit("/created_at", json!("rewritten"))),
        vec!["created_at".to_string()],
        "an immutable field refuses any change"
    );
}

mod nested {
    //! A plan whose explicit field sits two keyed sequences deep. Policy is
    //! judged on materialised documents, so only paths, identities, storage
    //! kinds and policies matter here.

    use clerkenwell_notebook::{
        GeneratedCollaborationConflict as Conflict, GeneratedCollaborationEntitySpec,
        GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind as Storage,
        GeneratedCollaborationValueCodec as Codec,
    };

    const fn field(
        path: &'static str,
        storage_kind: Storage,
        identity_path: Option<&'static str>,
        codec: Codec,
        conflict: Conflict,
    ) -> GeneratedCollaborationFieldSpec {
        GeneratedCollaborationFieldSpec {
            path,
            storage_kind,
            container: None,
            container_template: None,
            key: None,
            identity_path,
            identity_variable: None,
            order_container: None,
            item_container_template: None,
            metadata_container: None,
            metadata_container_template: None,
            metadata_key: None,
            codec,
            value_schema: None,
            required: true,
            conflict,
        }
    }

    static FIELDS: &[GeneratedCollaborationFieldSpec] = &[
        field(
            "columns",
            Storage::KeyedSequence,
            Some("column_id"),
            Codec::KeyedSequence,
            Conflict::Merge,
        ),
        field(
            "columns.*.cards",
            Storage::KeyedSequence,
            Some("card_id"),
            Codec::KeyedSequence,
            Conflict::Merge,
        ),
        field(
            "columns.*.cards.*.status",
            Storage::Scalar,
            None,
            Codec::String,
            Conflict::Explicit,
        ),
    ];

    pub static PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
        name: "Board",
        id_field: "board_id",
        substrate: "loro",
        schema_version: 1,
        migration_ids: &[],
        authoring_projection: "boards.authoringState",
        import_mutation: "board.importLoroUpdate",
        root_container: "board",
        fields: FIELDS,
    };
}

fn nested_board(first: &str, second: &str) -> Value {
    json!({
        "columns": [{
            "column_id": "c1",
            "cards": [
                { "card_id": "k1", "status": first },
                { "card_id": "k2", "status": second },
            ],
        }],
    })
}

#[test]
fn an_explicit_field_two_keyed_sequences_deep_is_judged_per_nested_item() {
    let base = nested_board("todo", "todo");

    let same_card = conflicting_field_paths(
        &nested::PLAN,
        &base,
        &nested_board("doing", "todo"),
        &nested_board("done", "todo"),
    );
    assert_eq!(same_card, ["columns[c1].cards[k1].status"]);

    let different_cards = conflicting_field_paths(
        &nested::PLAN,
        &base,
        &nested_board("doing", "todo"),
        &nested_board("todo", "done"),
    );
    assert!(different_cards.is_empty(), "{different_cards:?}");
}
