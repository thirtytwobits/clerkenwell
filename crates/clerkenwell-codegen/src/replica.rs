//! The document a replica holds: an entity's wire document, less some members
//! its schema requires.
//!
//! A replica never materialises a `derivedIdentity` member outside keyed
//! items, and materialises a `derivedRevision` member only when its caller
//! names a revision. It writes an object only beneath a field it writes, so it
//! leaves out an object no field beneath which is in every document.

use indexmap::IndexSet;

use crate::definition::{CollaborationEntity, CollaborationField, Definition};
use crate::error::Result;
use crate::validate::collaboration_path_presence;

/// Whether a replica may hold a document without the field.
fn materialised_optionally(field: &CollaborationField) -> bool {
    match field.storage_kind() {
        "derivedRevision" => true,
        "derivedIdentity" => !field.path.contains('*'),
        _ => false,
    }
}

/// The dotted paths, `*` standing for each item of a keyed sequence, at which
/// a replica may leave out a member the schema of `entity` requires, in the
/// order its fields declare them.
pub(crate) fn optional_paths(
    definition: &Definition,
    entity: CollaborationEntity,
) -> Result<IndexSet<String>> {
    let fields: Vec<CollaborationField> = entity.fields().collect();
    let declared = |path: &str| fields.iter().any(|field| field.path == path);
    let always_beneath = |path: &str| {
        let prefix = format!("{path}.");
        fields.iter().any(|field| {
            field.path.starts_with(&prefix) && field.required() && !materialised_optionally(field)
        })
    };
    let mut paths = IndexSet::new();
    for field in &fields {
        for (end, _) in field.path.match_indices('.') {
            let object = &field.path[..end];
            if object.ends_with('*')
                || declared(object)
                || always_beneath(object)
                || paths.contains(object)
            {
                continue;
            }
            let presence = collaboration_path_presence(definition, entity.name, object)?
                .expect("validation resolves every field path in the entity schema");
            if presence.required_in_parent {
                paths.insert(object.to_owned());
            }
        }
        if materialised_optionally(field) && field.required_in_parent() {
            paths.insert(field.path.to_owned());
        }
    }
    Ok(paths)
}
