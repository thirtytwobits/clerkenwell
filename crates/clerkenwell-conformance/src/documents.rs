//! Wire documents: plan paths expanded to JSON Pointers, the document a
//! replica holds of each, and the text fields a document holds.

use std::collections::HashMap;

use clerkenwell_schema::{
    GeneratedCollaborationEntitySpec, GeneratedCollaborationStorageKind,
    GeneratedCollaborationValueCodec,
};
use serde_json::{Map, Value};

/// The JSON Pointer for a dotted path without `*` segments.
pub fn pointer(path: &str) -> String {
    path.split('.')
        .map(|segment| format!("/{segment}"))
        .collect()
}

/// The JSON Pointers a dotted plan path names in `document`: each `*` stands
/// for every item of the array at that point.
pub fn pointers(document: &Value, path: &str) -> Vec<String> {
    let mut pointers = vec![String::new()];
    for segment in path.split('.') {
        pointers = pointers
            .into_iter()
            .flat_map(|prefix| {
                if segment != "*" {
                    return vec![format!("{prefix}/{segment}")];
                }
                let count = document
                    .pointer(&prefix)
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                (0..count)
                    .map(|index| format!("{prefix}/{index}"))
                    .collect()
            })
            .collect();
    }
    pointers
}

/// Sets the value at `pointer`, whose parent must exist.
pub fn set(document: &mut Value, pointer: &str, value: Value) {
    let (parent, key) = pointer.rsplit_once('/').expect("a pointer below the root");
    match document
        .pointer_mut(parent)
        .unwrap_or_else(|| panic!("{parent} exists"))
    {
        Value::Object(object) => {
            object.insert(key.to_string(), value);
        }
        Value::Array(items) => {
            items[key.parse::<usize>().expect("an array index")] = value;
        }
        other => panic!("{pointer} has no container parent: {other}"),
    }
}

/// Removes the object member at `pointer`.
pub fn remove(document: &mut Value, pointer: &str) {
    let (parent, key) = pointer.rsplit_once('/').expect("a pointer below the root");
    document
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("{parent} is an object"))
        .remove(key);
}

/// `wire`, a document of `plan`'s entity, as a replica materialises it at
/// `revision`. Outside keyed items a replica holds no `derivedIdentity`
/// member, and gives each `derivedRevision` member `revision`; inside them it
/// holds no `derivedRevision` member. It leaves out an optional member whose
/// value is empty, unless the member stores a structured map or document, and
/// keeps a keyed sequence `wire` holds even when empty. It writes an object
/// only beneath a field it holds, and gives a present object each member that
/// object requires but `wire` leaves out or holds empty, as its codec's empty
/// form.
pub fn held_document(
    plan: &GeneratedCollaborationEntitySpec,
    wire: &Value,
    revision: &str,
) -> Value {
    let mut held = held_node(plan, "", wire, true);
    for field in plan.fields.iter().filter(|field| {
        field.storage_kind == GeneratedCollaborationStorageKind::DerivedRevision
            && !field.path.contains('*')
    }) {
        let mut cursor = &mut held;
        let segments = field.path.split('.').collect::<Vec<_>>();
        let (last, parents) = segments.split_last().expect("a field path");
        for segment in parents {
            cursor = cursor
                .as_object_mut()
                .expect("an object")
                .entry(*segment)
                .or_insert_with(|| Value::Object(Map::new()));
        }
        cursor
            .as_object_mut()
            .expect("an object")
            .insert((*last).to_string(), Value::String(revision.to_string()));
    }
    held
}

/// `node`, at plan path `prefix`, as a replica holds it. A root or keyed item
/// is `always_present`; any other object is present only when a member is.
fn held_node(
    plan: &GeneratedCollaborationEntitySpec,
    prefix: &str,
    node: &Value,
    always_present: bool,
) -> Value {
    let Value::Object(members) = node else {
        return node.clone();
    };
    let mut held = Map::new();
    for (name, value) in members {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        let field = plan.fields.iter().find(|field| field.path == path);
        let member = match (field.map(|field| field.storage_kind), value) {
            (Some(GeneratedCollaborationStorageKind::DerivedIdentity), _)
                if !path.contains('*') =>
            {
                None
            }
            (Some(GeneratedCollaborationStorageKind::DerivedRevision), _) => None,
            (Some(GeneratedCollaborationStorageKind::KeyedSequence), Value::Array(items)) => {
                let item_prefix = format!("{path}.*");
                Some(Value::Array(
                    items
                        .iter()
                        .map(|item| held_node(plan, &item_prefix, item, true))
                        .collect(),
                ))
            }
            (Some(kind), value)
                if is_empty_optional_value(value)
                    && !field.is_some_and(|field| field.required)
                    && !is_object_storage_kind(kind) =>
            {
                None
            }
            (None, Value::Object(_))
                if plan
                    .fields
                    .iter()
                    .any(|field| field.path.starts_with(&format!("{path}."))) =>
            {
                Some(held_node(plan, &path, value, false)).filter(|object| {
                    object
                        .as_object()
                        .is_some_and(|members| !members.is_empty())
                })
            }
            _ => Some(value.clone()),
        };
        if let Some(member) = member {
            held.insert(name.clone(), member);
        }
    }
    if always_present || !held.is_empty() {
        complete_required_members(plan, prefix, &mut held);
    }
    Value::Object(held)
}

/// Gives `node`, the present object at plan path `prefix`, each member it
/// requires but does not hold, as that member's codec's empty form.
fn complete_required_members(
    plan: &GeneratedCollaborationEntitySpec,
    prefix: &str,
    node: &mut Map<String, Value>,
) {
    for field in plan
        .fields
        .iter()
        .filter(|field| field.required_in_parent && !field.required)
    {
        let (parent, name) = field.path.rsplit_once('.').unwrap_or(("", field.path));
        if parent != prefix || node.contains_key(name) {
            continue;
        }
        let empty = match field.codec {
            GeneratedCollaborationValueCodec::String
            | GeneratedCollaborationValueCodec::OptionalString => Value::String(String::new()),
            GeneratedCollaborationValueCodec::StringList
            | GeneratedCollaborationValueCodec::KeyedSequence => Value::Array(Vec::new()),
            GeneratedCollaborationValueCodec::StructuredJson
                if !is_object_storage_kind(field.storage_kind) =>
            {
                Value::Array(Vec::new())
            }
            _ => continue,
        };
        node.insert(name.to_owned(), empty);
    }
}

/// Whether a replica reads `value` as absent from an optional member.
fn is_empty_optional_value(value: &Value) -> bool {
    value.as_array().is_some_and(Vec::is_empty)
        || value.as_object().is_some_and(Map::is_empty)
        || value.as_str().is_some_and(str::is_empty)
        || value.is_null()
}

/// Whether a member stores its value whole as an object, which a replica
/// keeps even when empty.
fn is_object_storage_kind(kind: GeneratedCollaborationStorageKind) -> bool {
    matches!(
        kind,
        GeneratedCollaborationStorageKind::StructuredMap
            | GeneratedCollaborationStorageKind::StructuredDocument
    )
}

/// One declared text field of a document: the plan path, the identities of
/// the keyed items that enclose it, and the pointer to its string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTarget {
    pub field: &'static str,
    pub identities: HashMap<String, String>,
    pub pointer: String,
}

/// Every text field instance `document` holds.
pub fn text_targets(
    plan: &'static GeneratedCollaborationEntitySpec,
    document: &Value,
) -> Vec<TextTarget> {
    let mut targets = Vec::new();
    for field in plan
        .fields
        .iter()
        .filter(|field| field.storage_kind == GeneratedCollaborationStorageKind::Text)
    {
        for (at, identities) in instances(plan, document, field.path) {
            if document.pointer(&at).is_none() {
                continue;
            }
            let pointer = if field.codec == GeneratedCollaborationValueCodec::PropertyText {
                format!("{at}/value")
            } else {
                at
            };
            targets.push(TextTarget {
                field: field.path,
                identities,
                pointer,
            });
        }
    }
    targets
}

/// Every instance of `path` in `document`, with the identity of each keyed
/// item enclosing it under that sequence's identity variable.
fn instances(
    plan: &GeneratedCollaborationEntitySpec,
    document: &Value,
    path: &str,
) -> Vec<(String, HashMap<String, String>)> {
    let mut found = vec![(String::new(), HashMap::new())];
    let mut declared = Vec::new();
    for segment in path.split('.') {
        if segment == "*" {
            let sequence_path = declared.join(".");
            let sequence = plan
                .fields
                .iter()
                .find(|field| {
                    field.path == sequence_path
                        && field.storage_kind == GeneratedCollaborationStorageKind::KeyedSequence
                })
                .unwrap_or_else(|| panic!("{}.{sequence_path} is a keyed sequence", plan.name));
            let identity_path = sequence.identity_path.expect("a keyed sequence identity");
            let variable = sequence.identity_variable.unwrap_or(identity_path);
            found = found
                .into_iter()
                .flat_map(|(prefix, identities)| {
                    let items = document
                        .pointer(&prefix)
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    items
                        .into_iter()
                        .enumerate()
                        .map(|(index, item)| {
                            let mut identities = identities.clone();
                            let identity = item
                                .pointer(&pointer(identity_path))
                                .and_then(Value::as_str)
                                .expect("a keyed item identity");
                            identities.insert(variable.to_string(), identity.to_string());
                            (format!("{prefix}/{index}"), identities)
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
        } else {
            found = found
                .into_iter()
                .map(|(prefix, identities)| (format!("{prefix}/{segment}"), identities))
                .collect();
        }
        declared.push(segment);
    }
    found
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clerkenwell_doc::CollaborationReplica;
    use clerkenwell_notebook::GENERATED_COLLABORATION_SPECS;
    use serde_json::json;

    use super::*;
    use crate::corpus::Corpus;

    const REVISION: &str = "replica:held";

    /// The notebook's plan and fixture document for `entity`.
    fn notebook(entity: &str) -> (&'static GeneratedCollaborationEntitySpec, Value) {
        let corpus = Corpus::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../clerkenwell-notebook/generated/notebook.collaboration.fixtures.json"),
        );
        let fixture = corpus
            .entities()
            .iter()
            .find(|fixture| fixture.entity == entity)
            .expect("a fixture of the entity");
        let plan = GENERATED_COLLABORATION_SPECS
            .iter()
            .find(|plan| plan.name == entity)
            .expect("a plan of the entity");
        (plan, fixture.wire_document.clone())
    }

    /// What a Rust replica seeded with `wire` materialises at the revision.
    fn materialised(plan: &'static GeneratedCollaborationEntitySpec, wire: &Value) -> Value {
        CollaborationReplica::from_document(plan, wire, 1)
            .and_then(|replica| replica.materialized_document(REVISION))
            .expect("the replica materialises")
    }

    #[test]
    fn a_held_document_is_what_a_replica_materialises() {
        for entity in ["Note", "Board"] {
            let (plan, wire) = notebook(entity);

            assert_eq!(
                held_document(plan, &wire, REVISION),
                materialised(plan, &wire)
            );
        }
    }

    #[test]
    fn a_held_document_derives_its_root_identity_and_revision() {
        let (plan, mut wire) = notebook("Note");
        wire["note_id"] = json!("written-by-a-client");
        wire["etag"] = json!("written-by-a-client");

        let held = held_document(plan, &wire, REVISION);

        assert_eq!(held, materialised(plan, &wire));
        assert!(held.get("note_id").is_none(), "{held}");
        assert_eq!(held["etag"], REVISION);
    }

    #[test]
    fn a_held_document_leaves_out_an_object_holding_no_field() {
        let (plan, mut wire) = notebook("Note");
        wire["cover_image"] = json!({});

        let held = held_document(plan, &wire, REVISION);

        assert_eq!(held, materialised(plan, &wire));
        assert!(held.get("cover_image").is_none(), "{held}");
    }

    #[test]
    fn a_held_document_leaves_out_an_empty_optional_value() {
        let (plan, mut wire) = notebook("Note");
        wire["summary"] = json!("");
        wire["estimate"] = Value::Null;

        let held = held_document(plan, &wire, REVISION);

        assert_eq!(held, materialised(plan, &wire));
        assert!(held.get("summary").is_none(), "{held}");
        assert!(held.get("estimate").is_none(), "{held}");
    }

    #[test]
    fn a_held_document_restores_a_member_its_present_parent_requires() {
        let (plan, mut wire) = notebook("Note");
        wire["source"] = json!({ "url": "", "excerpt": "Quoted." });

        let held = held_document(plan, &wire, REVISION);

        assert_eq!(held, materialised(plan, &wire));
        assert!(held["source"].get("url").is_some(), "{held}");
    }

    #[test]
    fn a_held_document_keeps_an_empty_keyed_sequence_its_document_holds() {
        let (plan, mut wire) = notebook("Board");
        wire["archive"] = json!([]);

        let held = held_document(plan, &wire, REVISION);

        assert_eq!(held, materialised(plan, &wire));
        assert!(held.get("archive").is_some(), "{held}");
    }

    #[test]
    fn a_held_document_keeps_each_keyed_item_identity() {
        let (plan, wire) = notebook("Board");

        let held = held_document(plan, &wire, REVISION);

        assert_eq!(
            held["columns"][0]["column_id"],
            wire["columns"][0]["column_id"]
        );
    }
}
