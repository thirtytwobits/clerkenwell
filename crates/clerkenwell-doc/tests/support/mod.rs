//! The notebook's plans and generated collaboration fixtures, and replica
//! helpers shared by the replica tests.
#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clerkenwell_doc::loro::{ExportMode, LoroDoc};
use clerkenwell_doc::LoroAuthoringDocument;
use clerkenwell_notebook::{
    GeneratedCollaborationEntitySpec, GeneratedCollaborationFieldSpec,
    GeneratedCollaborationStorageKind, BOARD_COLLABORATION_SPEC, NOTE_COLLABORATION_SPEC,
};
use serde_json::{json, Value};

pub const NOTE: &GeneratedCollaborationEntitySpec = &NOTE_COLLABORATION_SPEC;
pub const BOARD: &GeneratedCollaborationEntitySpec = &BOARD_COLLABORATION_SPEC;

/// The revision every materialisation in these tests is read at.
pub const REVISION: &str = "loro:test";

pub fn corpus() -> Value {
    serde_json::from_str(include_str!(
        "../../../clerkenwell-notebook/generated/notebook.collaboration.fixtures.json"
    ))
    .expect("the notebook collaboration fixtures parse")
}

/// The generated fixture for one entity.
pub fn fixture(entity: &str) -> Value {
    corpus()["entities"]
        .as_array()
        .expect("entity fixtures")
        .iter()
        .find(|fixture| fixture["entity"] == entity)
        .unwrap_or_else(|| panic!("a {entity} fixture"))
        .clone()
}

/// The fixture's sample document for `plan`.
pub fn wire_document(plan: &GeneratedCollaborationEntitySpec) -> Value {
    fixture(plan.name)["wireDocument"].clone()
}

pub fn field(
    plan: &'static GeneratedCollaborationEntitySpec,
    path: &str,
) -> &'static GeneratedCollaborationFieldSpec {
    plan.fields
        .iter()
        .find(|field| field.path == path)
        .unwrap_or_else(|| panic!("{}.{path} is declared", plan.name))
}

/// `document` as a materialisation at `revision` reads it: every derived
/// revision field holds the revision.
pub fn at_revision(
    plan: &GeneratedCollaborationEntitySpec,
    document: &Value,
    revision: &str,
) -> Value {
    let mut expected = document.clone();
    for field in plan.fields.iter().filter(|field| {
        field.storage_kind == GeneratedCollaborationStorageKind::DerivedRevision
            && !field.path.contains('*')
    }) {
        set(&mut expected, &pointer(field.path), json!(revision));
    }
    expected
}

pub fn seed(
    plan: &'static GeneratedCollaborationEntitySpec,
    document: &Value,
) -> LoroAuthoringDocument {
    LoroAuthoringDocument::from_document(plan, document).expect("seed replica")
}

pub fn hydrate(
    plan: &'static GeneratedCollaborationEntitySpec,
    update: &str,
) -> LoroAuthoringDocument {
    LoroAuthoringDocument::from_versioned_update_base64(plan, plan.schema_version, update)
        .expect("hydrate replica")
}

pub fn read(replica: &LoroAuthoringDocument) -> Value {
    replica
        .materialized_document(REVISION)
        .expect("materialise")
}

/// What `replica` reads as once its whole history is loaded into a new replica.
pub fn reread(
    plan: &'static GeneratedCollaborationEntitySpec,
    replica: &LoroAuthoringDocument,
) -> Value {
    read(&hydrate(
        plan,
        &replica.export_update_base64().expect("export"),
    ))
}

/// A raw Loro document holding `replica`'s history.
pub fn raw(replica: &LoroAuthoringDocument) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.import(
        &BASE64
            .decode(replica.export_update_base64().expect("export"))
            .expect("base64"),
    )
    .expect("import");
    doc
}

pub fn export_all(doc: &LoroDoc) -> String {
    BASE64.encode(doc.export(ExportMode::all_updates()).expect("export"))
}

/// Seeds `base`, lets each writer rewrite the whole document from it
/// independently, and returns what a replica that received every rewrite holds
/// in each import order.
pub fn merge_rewrites(
    plan: &'static GeneratedCollaborationEntitySpec,
    base: &Value,
    rewrites: &[Value],
) -> (Value, Value) {
    let seeded = seed(plan, base);
    let frontier = seeded.accepted_frontier_base64();
    let update = seeded.export_update_base64().expect("export seed");
    let updates = rewrites
        .iter()
        .map(|rewrite| {
            let mut writer = hydrate(plan, &update);
            writer.replace_document(rewrite).expect("rewrite");
            writer
                .export_incremental_update_base64(&frontier)
                .expect("export rewrite")
        })
        .collect::<Vec<_>>();
    let merge = |order: &mut dyn Iterator<Item = &String>| {
        let merged = hydrate(plan, &update);
        for rewrite in order {
            merged
                .import_versioned_update_base64(plan.schema_version, rewrite)
                .expect("merge rewrite");
        }
        read(&merged)
    };
    (merge(&mut updates.iter()), merge(&mut updates.iter().rev()))
}

/// Merges two rewrites of `base` in both import orders, requires the orders to
/// agree, and returns the merged document.
pub fn converge(
    plan: &'static GeneratedCollaborationEntitySpec,
    base: &Value,
    one: &Value,
    two: &Value,
) -> Value {
    let (forward, reverse) = merge_rewrites(plan, base, &[one.clone(), two.clone()]);
    assert_eq!(forward, reverse, "both import orders converge");
    forward
}

/// The JSON Pointer for a dotted wire path without `*` segments.
pub fn pointer(path: &str) -> String {
    path.split('.')
        .map(|segment| format!("/{segment}"))
        .collect()
}

/// The JSON Pointers a dotted wire path names in `document`: each `*` stands
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

pub fn set(document: &mut Value, pointer: &str, value: Value) {
    let (parent, key) = pointer.rsplit_once('/').expect("a pointer below the root");
    let parent = if parent.is_empty() {
        document
    } else {
        document
            .pointer_mut(parent)
            .unwrap_or_else(|| panic!("{parent} exists"))
    };
    match parent {
        Value::Object(object) => {
            object.insert(key.to_string(), value);
        }
        Value::Array(items) => {
            items[key.parse::<usize>().expect("an array index")] = value;
        }
        other => panic!("{pointer} has no container parent: {other}"),
    }
}

pub fn remove(document: &mut Value, pointer: &str) {
    let (parent, key) = pointer.rsplit_once('/').expect("a pointer below the root");
    document
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("{parent} is an object"))
        .remove(key);
}

pub fn with(document: &Value, pointer: &str, value: Value) -> Value {
    let mut edited = document.clone();
    set(&mut edited, pointer, value);
    edited
}

pub fn items<'a>(document: &'a Value, pointer: &str) -> &'a Vec<Value> {
    document
        .pointer(pointer)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{pointer} is an array in {document}"))
}

pub fn items_mut<'a>(document: &'a mut Value, pointer: &str) -> &'a mut Vec<Value> {
    document
        .pointer_mut(pointer)
        .and_then(Value::as_array_mut)
        .unwrap_or_else(|| panic!("{pointer} is an array"))
}

/// The identities of the items of the array at `pointer`, in order.
pub fn identities(document: &Value, pointer: &str, identity: &str) -> Vec<String> {
    items(document, pointer)
        .iter()
        .map(|item| item[identity].as_str().expect("an identity").to_string())
        .collect()
}

pub fn card(card_id: &str, text: &str) -> Value {
    json!({
        "card_id": card_id,
        "text": text,
        "done": false,
        "note": { "$mime": "text/markdown", "value": format!("Notes on {text}.") },
    })
}

pub fn column(column_id: &str, title: &str, cards: Vec<Value>) -> Value {
    json!({ "column_id": column_id, "title": title, "cards": cards })
}

/// A board whose columns hold different cards.
pub fn board() -> Value {
    json!({
        "name": "Release",
        "columns": [
            column("todo", "To do", vec![card("draft", "Draft the notes"), card("review", "Review")]),
            column("done", "Done", vec![card("plan", "Plan the release")]),
        ],
    })
}
