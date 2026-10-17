//! The document a replica holds: an entity's wire document, less some members
//! its schema requires.
//!
//! A replica never materialises a `derivedIdentity` member outside keyed
//! items, and materialises a `derivedRevision` member only when its caller
//! names a revision. It writes an object only beneath a field it writes, so it
//! leaves out an object no field beneath which is in every document, and an
//! object no field lies beneath at all.

use indexmap::IndexSet;

use crate::definition::{CollaborationEntity, CollaborationField, Definition};
use crate::error::Result;
use crate::json::Object;
use crate::schema::{has_type, object_at};
use crate::validate::{collaboration_path_presence, dereference};

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
    let root = definition
        .entity(entity.name)
        .expect("validation requires the collaboration entity to exist")
        .schema();
    let mut fieldless = IndexSet::new();
    fieldless_objects(definition, root, "", &fields, &mut fieldless)?;
    for object in fieldless {
        let presence = collaboration_path_presence(definition, entity.name, &object)?
            .expect("the walk follows the entity schema");
        if presence.required_in_parent {
            paths.insert(object);
        }
    }
    Ok(paths)
}

/// Every object member of `raw_schema`, at plan path `prefix`, that no field
/// is declared at or beneath, `*` standing for each item of a keyed sequence.
/// A member a field stores whole is the field's value, not such an object.
fn fieldless_objects(
    definition: &Definition,
    raw_schema: &Object,
    prefix: &str,
    fields: &[CollaborationField],
    found: &mut IndexSet<String>,
) -> Result<()> {
    let schema = dereference(definition, raw_schema)?;
    let Some(properties) = has_type(schema, "object")
        .then(|| object_at(schema, "properties"))
        .flatten()
    else {
        return Ok(());
    };
    for (member, member_schema) in properties.iter() {
        let Some(member_schema) = member_schema.as_object() else {
            continue;
        };
        let path = if prefix.is_empty() {
            member.to_owned()
        } else {
            format!("{prefix}.{member}")
        };
        if let Some(field) = fields.iter().find(|field| field.path == path) {
            if field.storage_kind() == "keyedSequence" {
                let sequence = dereference(definition, member_schema)?;
                if let Some(items) = has_type(sequence, "array")
                    .then(|| object_at(sequence, "items"))
                    .flatten()
                {
                    fieldless_objects(definition, items, &format!("{path}.*"), fields, found)?;
                }
            }
            continue;
        }
        let beneath = format!("{path}.");
        if fields.iter().any(|field| field.path.starts_with(&beneath)) {
            fieldless_objects(definition, member_schema, &path, fields, found)?;
        } else if has_type(dereference(definition, member_schema)?, "object") {
            found.insert(path);
        }
    }
    Ok(())
}
