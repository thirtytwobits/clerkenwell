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
    let mut values = BTreeMap::new();
    collect_field_values(plan, "", field.path, document, "", &mut values);
    values
}

/// Collects the values `relative` addresses in `scope`, whose declared path
/// prefix is `declared_prefix`, descending through every keyed sequence on the
/// way. Each value is keyed by its field path with every enclosing item named
/// by its identity: `sequence[identity].nested[identity].field`.
fn collect_field_values(
    plan: &GeneratedCollaborationEntitySpec,
    declared_prefix: &str,
    relative: &str,
    scope: &Value,
    key_prefix: &str,
    values: &mut BTreeMap<String, Value>,
) {
    let Some((sequence_relative, rest)) = relative.split_once(".*.") else {
        values.insert(
            format!("{key_prefix}{relative}"),
            value_at_path(scope, relative)
                .cloned()
                .unwrap_or(Value::Null),
        );
        return;
    };
    let sequence_path = format!("{declared_prefix}{sequence_relative}");
    let identity_path = plan
        .fields
        .iter()
        .find(|candidate| {
            candidate.path == sequence_path
                && candidate.storage_kind == GeneratedCollaborationStorageKind::KeyedSequence
        })
        .and_then(|candidate| candidate.identity_path);
    let (Some(identity_path), Some(items)) = (
        identity_path,
        value_at_path(scope, sequence_relative).and_then(Value::as_array),
    ) else {
        return;
    };
    for item in items {
        let Some(identity) = value_at_path(item, identity_path).and_then(Value::as_str) else {
            continue;
        };
        collect_field_values(
            plan,
            &format!("{sequence_path}.*."),
            rest,
            item,
            &format!("{key_prefix}{sequence_relative}[{identity}]."),
            values,
        );
    }
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .filter(|segment| !segment.is_empty())
        .try_fold(value, |current, segment| current.get(segment))
}
