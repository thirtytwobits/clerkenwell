//! Field conflict policy evaluation over materialised documents.
//!
//! A concurrent edit is judged field by field: `immutable` fields may not
//! change at all, `explicit` fields refuse two different changes to one value,
//! and `merge` and `lastWriterWins` fields always accept. Fields inside a keyed
//! sequence are judged per item, addressed by the item's identity.

use std::collections::{BTreeMap, BTreeSet};

use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind,
};
use serde_json::Value;

/// The declared field paths an edit from `base` to `client` changes against
/// their conflict policy, given that the accepted document has meanwhile
/// become `current`. Keyed item fields are reported as
/// `sequence[identity].field`.
pub fn conflicting_field_paths(
    plan: &GeneratedCollaborationEntitySpec,
    base: &Value,
    client: &Value,
    current: &Value,
) -> Vec<String> {
    let mut conflicts = BTreeSet::new();
    for field in plan.fields {
        if !matches!(
            field.conflict,
            GeneratedCollaborationConflict::Explicit | GeneratedCollaborationConflict::Immutable
        ) || matches!(
            field.storage_kind,
            GeneratedCollaborationStorageKind::DerivedIdentity
                | GeneratedCollaborationStorageKind::DerivedRevision
        ) {
            continue;
        }
        let base_values = field_values(plan, field, base);
        let client_values = field_values(plan, field, client);
        let current_values = field_values(plan, field, current);
        let keys = base_values
            .keys()
            .chain(client_values.keys())
            .chain(current_values.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for key in keys {
            let base_value = base_values.get(&key).unwrap_or(&Value::Null);
            let client_value = client_values.get(&key).unwrap_or(&Value::Null);
            let current_value = current_values.get(&key).unwrap_or(&Value::Null);
            let conflict = match field.conflict {
                GeneratedCollaborationConflict::Immutable => client_value != base_value,
                GeneratedCollaborationConflict::Explicit => {
                    client_value != base_value
                        && current_value != base_value
                        && client_value != current_value
                }
                GeneratedCollaborationConflict::Merge
                | GeneratedCollaborationConflict::LastWriterWins => false,
            };
            if conflict {
                conflicts.insert(key);
            }
        }
    }
    conflicts.into_iter().collect()
}

fn field_values(
    plan: &GeneratedCollaborationEntitySpec,
    field: &GeneratedCollaborationFieldSpec,
    document: &Value,
) -> BTreeMap<String, Value> {
    let Some((sequence_path, item_path)) = field.path.split_once(".*") else {
        return BTreeMap::from([(
            field.path.to_string(),
            value_at_path(document, field.path)
                .cloned()
                .unwrap_or(Value::Null),
        )]);
    };
    let identity_path = plan
        .fields
        .iter()
        .find(|candidate| {
            candidate.path == sequence_path
                && candidate.storage_kind == GeneratedCollaborationStorageKind::KeyedSequence
        })
        .and_then(|candidate| candidate.identity_path);
    let Some(identity_path) = identity_path else {
        return BTreeMap::new();
    };
    let Some(items) = value_at_path(document, sequence_path).and_then(Value::as_array) else {
        return BTreeMap::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let identity = value_at_path(item, identity_path)?.as_str()?.to_string();
            let suffix = item_path.strip_prefix('.').unwrap_or(item_path);
            let value = if suffix.is_empty() {
                item.clone()
            } else {
                value_at_path(item, suffix).cloned().unwrap_or(Value::Null)
            };
            Some((format!("{sequence_path}[{identity}]{item_path}"), value))
        })
        .collect()
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .filter(|segment| !segment.is_empty())
        .try_fold(value, |current, segment| current.get(segment))
}
