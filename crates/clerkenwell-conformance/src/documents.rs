//! Wire documents: plan paths expanded to JSON Pointers, the client naming
//! TypeScript replicas read and write, and the text fields a document holds.

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

/// `snake_case` segments as TypeScript client documents spell them.
pub fn snake_to_camel(value: &str) -> String {
    let mut camel = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match characters.peek() {
            Some(next) if character == '_' && next.is_ascii_lowercase() => {
                camel.push(next.to_ascii_uppercase());
                characters.next();
            }
            _ => camel.push(character),
        }
    }
    camel
}

/// `wire` as a TypeScript client document holds it. Declared field paths and
/// the keys of structured list entries are camelCase; the contents of maps
/// and structured documents keep their keys.
pub fn client_document(plan: &GeneratedCollaborationEntitySpec, wire: &Value) -> Value {
    client_node(plan, "", wire)
}

fn client_node(plan: &GeneratedCollaborationEntitySpec, prefix: &str, node: &Value) -> Value {
    let mut client = Value::Object(Map::new());
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
        let value = match (field.storage_kind, value) {
            (GeneratedCollaborationStorageKind::KeyedSequence, Value::Array(items)) => {
                let item_prefix = format!("{}.*.", field.path);
                Value::Array(
                    items
                        .iter()
                        .map(|item| client_node(plan, &item_prefix, item))
                        .collect(),
                )
            }
            (GeneratedCollaborationStorageKind::StructuredList, Value::Array(entries)) => {
                Value::Array(entries.iter().map(camel_case_keys).collect())
            }
            _ => value.clone(),
        };
        let mut cursor = &mut client;
        let segments = relative.split('.').map(snake_to_camel).collect::<Vec<_>>();
        let (last, parents) = segments.split_last().expect("a field path");
        for segment in parents {
            cursor = cursor
                .as_object_mut()
                .expect("an object")
                .entry(segment.as_str())
                .or_insert_with(|| Value::Object(Map::new()));
        }
        cursor
            .as_object_mut()
            .expect("an object")
            .insert(last.clone(), value);
    }
    client
}

fn camel_case_keys(entry: &Value) -> Value {
    match entry {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (snake_to_camel(key), value.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
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
