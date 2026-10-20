//! Definition validation: the meta-schema, then the invariants JSON Schema
//! cannot state, each refusal naming the definition path at fault.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use indexmap::IndexSet;

use crate::collate::code_unit_compare;
use crate::definition::{CollaborationEntity, CollaborationField, Definition, Fields, Projection};
use crate::error::{refuse, Result};
use crate::json::{string_literal, Json, Object};
use crate::names::{name_constant_identifier, pascal_identifier};
use crate::rust::collect_rust_property_enums;
use crate::schema::{
    has_type, object_at, other_members_schema, primitive_union_types, read_string_array,
    record_values_schema, resolve_ref, schema_ref_name, value_meets_bounds,
};

/// The meta-schema every definition document must satisfy.
pub const META_SCHEMA: &str = include_str!("projection-definition.schema.json");

pub(crate) fn check_meta_schema(document: &Json) -> Result<()> {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    let validator = VALIDATOR.get_or_init(|| {
        let schema: serde_json::Value =
            serde_json::from_str(META_SCHEMA).expect("the embedded meta-schema is JSON");
        jsonschema::validator_for(&schema).expect("the embedded meta-schema is a valid schema")
    });
    let instance = document.to_serde();
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|error| {
            let path = error.instance_path().as_str();
            format!("{} {error}", if path.is_empty() { "/" } else { path })
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        refuse(format!(
            "Projection definition does not match the projection definition meta-schema: {}",
            errors.join("; ")
        ))
    }
}

pub(crate) fn check_semantics(definition: &Definition) -> Result<()> {
    let entity_names = definition.entity_names();
    let collaboration = definition.collaboration();

    if definition.projection_count() == 0 {
        return refuse(
            "projections must declare a projection, or collaboration an entity whose authoring state is one.".to_owned(),
        );
    }

    check_reference_chains(definition)?;

    // Every `$defs` entry must be renderable, and generated type names must be
    // collision-free after identifier normalisation.
    let mut generated_type_names: HashMap<String, &str> = HashMap::new();
    for (definition_name, schema) in definition.defs().iter() {
        let generated_name = pascal_identifier(definition_name);
        if let Some(previous) = generated_type_names.get(&generated_name) {
            return refuse(format!(
                "$defs.{definition_name} and $defs.{previous} both generate {generated_name}."
            ));
        }
        generated_type_names.insert(generated_name, definition_name);
        check_schema_node(
            definition,
            Some(schema),
            &format!("$defs.{definition_name}"),
        )?;
    }

    // Entity ids and revision fields must name properties of the entity schema.
    for entity in definition.entities() {
        let name = entity.name;
        let schema_name = schema_ref_name(
            definition,
            entity.schema(),
            &format!("entities.{name}.schema"),
        )?;
        let entity_schema = definition.def(&schema_name).expect("resolved above");
        check_entity_schema_property(entity_schema, entity.id(), &format!("entities.{name}.id"))?;
        let revision_field = entity.revision().opt_str("field");
        if entity.revision_kind() == "contentHash" {
            check_entity_schema_property(
                entity_schema,
                revision_field.expect("the meta-schema requires a contentHash field"),
                &format!("entities.{name}.revision.field"),
            )?;
        } else if entity.revision().get("field").is_some() {
            return refuse(format!(
                "entities.{name}.revision.field is not supported for replica revisions."
            ));
        }
        let kind = entity.authoring_kind();
        if kind == "collaborative" && entity.revision_kind() != "replica" {
            return refuse(format!(
                "entities.{name}.authoring.kind \"collaborative\" requires a replica revision."
            ));
        }
        let entity_mutations: HashSet<&str> = definition
            .mutations()
            .filter(|mutation| mutation.touches().contains(&name))
            .map(|mutation| mutation.name)
            .collect();
        for mutation in entity.planning_mutations() {
            if !entity_mutations.contains(mutation) {
                return refuse(format!(
                    "entities.{name}.authoring.planningMutations references mutation {} that does not touch {name}.",
                    string_literal(mutation)
                ));
            }
        }
    }

    for collaboration_entity in collaboration.entities() {
        let name = collaboration_entity.name;
        let Some(entity) = definition.entity(name) else {
            return refuse(format!(
                "collaboration.entities.{name} references unknown entity {}.",
                string_literal(name)
            ));
        };
        if entity.revision_kind() != "replica" || entity.authoring_kind() != "collaborative" {
            return refuse(format!(
                "collaboration.entities.{name} requires a collaborative entity with a replica revision."
            ));
        }
        check_collaboration_entity(definition, collaboration_entity)?;
    }
    for entity in definition.entities() {
        if entity.authoring_kind() == "collaborative" && collaboration.entity(entity.name).is_none()
        {
            return refuse(format!(
                "entities.{name} is collaborative but collaboration.entities.{name} is missing.",
                name = entity.name
            ));
        }
    }

    // Projection contract refs resolve eagerly so a stale name fails before
    // any generated output can drift.
    for projection in definition.projections() {
        let name = projection.name;
        schema_ref_name(
            definition,
            projection.params(),
            &format!("projections.{name}.params"),
        )?;
        schema_ref_name(
            definition,
            projection.snapshot(),
            &format!("projections.{name}.snapshot"),
        )?;
        schema_ref_name(
            definition,
            projection.patch(),
            &format!("projections.{name}.patch"),
        )?;
        for dependency in projection.depends_on() {
            if !entity_names.contains(&dependency) {
                return refuse(format!(
                    "projections.{name}.dependsOn references unknown entity {}.",
                    string_literal(dependency)
                ));
            }
        }
        check_materialization(definition, projection)?;
    }

    for mutation in definition.mutations() {
        let name = mutation.name;
        schema_ref_name(
            definition,
            mutation.params(),
            &format!("mutations.{name}.params"),
        )?;
        schema_ref_name(
            definition,
            mutation.result(),
            &format!("mutations.{name}.result"),
        )?;
        for touched in mutation.touches() {
            if !entity_names.contains(&touched) {
                return refuse(format!(
                    "mutations.{name}.touches references unknown entity {}.",
                    string_literal(touched)
                ));
            }
            if definition
                .entity(touched)
                .is_some_and(|entity| entity.authoring_kind() == "readOnly")
            {
                return refuse(format!(
                    "mutations.{name}.touches includes read-only entity {}.",
                    string_literal(touched)
                ));
            }
        }
    }

    for command in definition.commands() {
        let name = command.name;
        schema_ref_name(
            definition,
            command.params(),
            &format!("commands.{name}.params"),
        )?;
        schema_ref_name(
            definition,
            command.result(),
            &format!("commands.{name}.result"),
        )?;
        for touched in command.touches() {
            if !entity_names.contains(&touched) {
                return refuse(format!(
                    "commands.{name}.touches references unknown entity {}.",
                    string_literal(touched)
                ));
            }
            if definition
                .entity(touched)
                .is_some_and(|entity| entity.authoring_kind() == "readOnly")
            {
                return refuse(format!(
                    "commands.{name}.touches includes read-only entity {}.",
                    string_literal(touched)
                ));
            }
        }
    }

    check_generated_output_names(definition)?;

    // Projection, mutation and command names share a public API namespace.
    let mut public_names = HashSet::new();
    for name in definition
        .projections()
        .map(|projection| projection.name)
        .chain(definition.mutations().map(|mutation| mutation.name))
        .chain(definition.commands().map(|command| command.name))
    {
        if !public_names.insert(name) {
            return refuse(format!(
                "Projection, mutation and command names must be unique; {} is duplicated.",
                string_literal(name)
            ));
        }
    }
    Ok(())
}

fn materialization_patch_kinds(strategy: &str) -> &'static [&'static str] {
    match strategy {
        "keyedCollection" => &["remove", "reset", "upsert"],
        "sequencedText" => &["output", "reset"],
        "replaceOrRemove" => &["remove", "replace"],
        "replace" => &["replace"],
        "reset" => &["reset"],
        other => unreachable!("the meta-schema admits no strategy {other:?}"),
    }
}

fn check_materialization(definition: &Definition, projection: Projection) -> Result<()> {
    let name = projection.name;
    let context = format!("projections.{name}.materialization");
    let snapshot_name = schema_ref_name(
        definition,
        projection.snapshot(),
        &format!("projections.{name}.snapshot"),
    )?;
    let patch_name = schema_ref_name(
        definition,
        projection.patch(),
        &format!("projections.{name}.patch"),
    )?;
    let snapshot_schema = definition.def(&snapshot_name).expect("resolved above");
    let patch_schema = definition.def(&patch_name).expect("resolved above");
    let kind_schema =
        object_at(patch_schema, "properties").and_then(|properties| object_at(properties, "kind"));
    let mut kinds = read_string_array(
        kind_schema.and_then(|schema| schema.get("enum")),
        &format!("$defs.{patch_name}.properties.kind.enum"),
    )?;
    kinds.sort_by(|left, right| code_unit_compare(left, right));
    let materialization = projection.materialization();
    let strategy = materialization.strategy();
    let mut expected: Vec<&str> = materialization_patch_kinds(strategy).to_vec();
    if strategy == "replaceOrRemove" && truthy(materialization.setting("updateField")) {
        expected.push("update");
    }
    expected.sort_by(|left, right| code_unit_compare(left, right));
    if kinds != expected {
        return refuse(format!(
            "projections.{name}.materialization.strategy {} expects patch kinds {}, found {}.",
            string_literal(strategy),
            expected.join(", "),
            kinds.join(", ")
        ));
    }

    match strategy {
        "keyedCollection" => {
            let collection_field = materialization.required("collectionField");
            let item_identity_field = materialization.required("itemIdentityField");
            let collection_context = format!("{context}.collectionField");
            let identity_context = format!("{context}.itemIdentityField");
            let snapshot_collection =
                contract_property(snapshot_schema, collection_field, &collection_context)?;
            let patch_collection =
                contract_property(patch_schema, collection_field, &collection_context)?;
            let patch_item = contract_property(
                patch_schema,
                materialization.required("itemField"),
                &format!("{context}.itemField"),
            )?;
            contract_property(
                patch_schema,
                materialization.required("patchIdentityField"),
                &format!("{context}.patchIdentityField"),
            )?;
            check_array_item_property(
                definition,
                snapshot_collection,
                item_identity_field,
                &identity_context,
            )?;
            check_array_item_property(
                definition,
                patch_collection,
                item_identity_field,
                &identity_context,
            )?;
            check_schema_property(
                definition,
                patch_item,
                item_identity_field,
                &identity_context,
            )?;
        }
        "sequencedText" => {
            let snapshot_collection = contract_property(
                snapshot_schema,
                materialization.required("collectionField"),
                &format!("{context}.collectionField"),
            )?;
            check_array_item_property(
                definition,
                snapshot_collection,
                materialization.required("itemIdentityField"),
                &format!("{context}.itemIdentityField"),
            )?;
            contract_property(
                patch_schema,
                materialization.required("patchIdentityField"),
                &format!("{context}.patchIdentityField"),
            )?;
            contract_property(
                patch_schema,
                materialization.required("patchOutputField"),
                &format!("{context}.patchOutputField"),
            )?;
        }
        "replaceOrRemove" => {
            let update_field = materialization
                .setting("updateField")
                .filter(|field| !field.is_empty());
            let updates_field = materialization
                .setting("updatesField")
                .filter(|field| !field.is_empty());
            if update_field.is_some() != updates_field.is_some() {
                return refuse(format!(
                    "{context} requires both updateField and updatesField."
                ));
            }
            if let (Some(update_field), Some(updates_field)) = (update_field, updates_field) {
                check_update_fields(
                    definition,
                    snapshot_schema,
                    patch_schema,
                    update_field,
                    updates_field,
                    &context,
                )?;
            }
            check_snapshot_mode(snapshot_schema, patch_schema, materialization.raw, &context)?;
            if materialization.setting("removeMode") == Some("nullField") {
                contract_property(
                    snapshot_schema,
                    materialization.required("removeField"),
                    &format!("{context}.removeField"),
                )?;
            }
        }
        "replace" => {
            check_snapshot_mode(snapshot_schema, patch_schema, materialization.raw, &context)?
        }
        "reset" => check_patch_matches_snapshot(snapshot_schema, patch_schema, &context, &[])?,
        _ => unreachable!("strategy checked above"),
    }
    Ok(())
}

/// ECMAScript truthiness of an optional string.
fn truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn check_update_fields(
    definition: &Definition,
    snapshot_schema: &Object,
    patch_schema: &Object,
    update_field: &str,
    updates_field: &str,
    context: &str,
) -> Result<()> {
    let target = contract_property(
        snapshot_schema,
        update_field,
        &format!("{context}.updateField"),
    )?;
    let changes = contract_property(
        patch_schema,
        updates_field,
        &format!("{context}.updatesField"),
    )?;
    // A reference is followed by its last path segment.
    let follow = |schema: &Object| -> Option<Object> {
        match schema.get("$ref") {
            Some(Json::String(reference)) => {
                let name = reference
                    .rsplit('/')
                    .next()
                    .expect("split yields a segment");
                definition.def(name).cloned()
            }
            _ => Some(schema.clone()),
        }
    };
    let (Some(target_schema), Some(changes_schema)) = (follow(target), follow(changes)) else {
        return refuse(format!("{context} update fields must be objects."));
    };
    let (Some(target_properties), Some(changes_properties)) = (
        object_at(&target_schema, "properties"),
        object_at(&changes_schema, "properties"),
    ) else {
        return refuse(format!("{context} update fields must be objects."));
    };
    for (key, value) in changes_properties.iter() {
        if target_properties.get(key).map(Json::stringify) != Some(value.stringify()) {
            return refuse(format!(
                "{context} update property {key} must match the snapshot contract."
            ));
        }
    }
    Ok(())
}

fn check_snapshot_mode(
    snapshot_schema: &Object,
    patch_schema: &Object,
    materialization: &Object,
    context: &str,
) -> Result<()> {
    if materialization.opt_str("snapshotMode") == Some("field") {
        contract_property(
            patch_schema,
            materialization.str_field("snapshotField"),
            &format!("{context}.snapshotField"),
        )?;
        return Ok(());
    }
    let omit_fields = materialization
        .opt_strs_field("snapshotOmitFields")
        .unwrap_or_default();
    for omit_field in &omit_fields {
        if *omit_field == "kind" {
            return refuse(format!(
                "{context}.snapshotOmitFields must not include \"kind\"."
            ));
        }
        contract_property(
            patch_schema,
            omit_field,
            &format!("{context}.snapshotOmitFields"),
        )?;
    }
    check_patch_matches_snapshot(snapshot_schema, patch_schema, context, &omit_fields)
}

/// The declared property `property` of an object contract schema.
pub(crate) fn contract_property<'a>(
    schema: &'a Object,
    property: &str,
    context: &str,
) -> Result<&'a Object> {
    let found = has_type(schema, "object")
        .then(|| object_at(schema, "properties"))
        .flatten()
        .and_then(|properties| object_at(properties, property));
    match found {
        Some(found) => Ok(found),
        None => refuse(format!(
            "{context} references property {} that is not declared on the contract schema.",
            string_literal(property)
        )),
    }
}

fn check_array_item_property(
    definition: &Definition,
    schema: &Object,
    property: &str,
    context: &str,
) -> Result<()> {
    let items = has_type(schema, "array")
        .then(|| object_at(schema, "items"))
        .flatten();
    match items {
        Some(items) => check_schema_property(definition, items, property, context),
        None => refuse(format!("{context} requires an array contract.")),
    }
}

fn check_schema_property(
    definition: &Definition,
    schema: &Object,
    property: &str,
    context: &str,
) -> Result<()> {
    let resolved = if schema.get("$ref").is_none() {
        schema
    } else {
        resolve_ref(definition, schema, context)?
    };
    // A `json` payload is validated at its owning boundary; its properties are
    // invisible here.
    if has_type(resolved, "json") {
        return Ok(());
    }
    contract_property(resolved, property, context).map(|_| ())
}

fn check_patch_matches_snapshot(
    snapshot_schema: &Object,
    patch_schema: &Object,
    context: &str,
    omit_fields: &[&str],
) -> Result<()> {
    let mut snapshot_properties: Vec<&str> = object_at(snapshot_schema, "properties")
        .map(|properties| properties.keys().collect())
        .unwrap_or_default();
    snapshot_properties.sort_by(|left, right| code_unit_compare(left, right));
    let mut patch_properties: Vec<&str> = object_at(patch_schema, "properties")
        .map(|properties| {
            properties
                .keys()
                .filter(|property| *property != "kind" && !omit_fields.contains(property))
                .collect()
        })
        .unwrap_or_default();
    patch_properties.sort_by(|left, right| code_unit_compare(left, right));
    if snapshot_properties != patch_properties {
        return refuse(format!(
            "{context}.snapshotMode \"patch\" requires patch fields other than kind to match the snapshot contract."
        ));
    }
    Ok(())
}

fn check_collaboration_entity(
    definition: &Definition,
    collaboration: CollaborationEntity,
) -> Result<()> {
    let entity_name = collaboration.name;
    let context = format!("collaboration.entities.{entity_name}");
    let projection_depends = definition
        .projection(collaboration.authoring_projection())
        .is_some_and(|projection| projection.depends_on().contains(&entity_name));
    if !projection_depends {
        return refuse(format!(
            "{context}.authoringState.projection must name a projection depending on {entity_name}."
        ));
    }
    let import_mutation = collaboration.import_mutation();
    let mutation_touches = definition
        .mutation(import_mutation)
        .is_some_and(|mutation| mutation.touches().contains(&entity_name));
    if !mutation_touches {
        return refuse(format!(
            "{context}.authoringState.importMutation must name a mutation touching {entity_name}."
        ));
    }
    let entity = definition
        .entity(entity_name)
        .expect("checked by the caller");

    let schema_name = schema_ref_name(
        definition,
        entity.schema(),
        &format!("entities.{entity_name}.schema"),
    )?;
    let entity_schema = definition.def(&schema_name).expect("resolved above");
    if other_members_schema(entity_schema).is_some() {
        return refuse(format!(
            "entities.{entity_name}.schema must be a closed object: a collaborative entity's members are its fields."
        ));
    }
    let required_paths = collect_collaboration_schema_paths(definition, entity_schema, "")?;
    let fields: Vec<_> = collaboration.fields().collect();

    // Each member of a keyed sequence's item is held in a container of its
    // own, which an open object's or a record's members are not.
    for field in fields
        .iter()
        .filter(|field| field.storage_kind() == "keyedSequence")
    {
        let path = field.path;
        let items =
            collaboration_schema_at(definition, entity_schema, path)?.and_then(|(_, sequence)| {
                has_type(sequence, "array")
                    .then(|| object_at(sequence, "items"))
                    .flatten()
            });
        let Some(items) = items else {
            continue;
        };
        let items = dereference(definition, items)?;
        if !has_type(items, "object")
            || items.get("additionalProperties") != Some(&Json::Bool(false))
        {
            return refuse(format!(
                "{context}.fields.{path} is a keyed sequence, whose items must be closed objects."
            ));
        }
    }

    for field in &fields {
        let path = field.path;
        let Some(presence) = resolve_collaboration_schema_path(definition, entity_schema, path)?
        else {
            return refuse(format!(
                "{context}.fields.{path} does not resolve in {schema_name}."
            ));
        };
        if presence.required != field.required() {
            return refuse(format!(
                "{context}.fields.{path}.required must match the entity schema ({}).",
                presence.required
            ));
        }
        if presence.required_in_parent != field.required_in_parent() {
            return refuse(format!(
                "{context}.fields.{path}.requiredInParent must match the entity schema ({}).",
                presence.required_in_parent
            ));
        }
        check_collaboration_storage(&format!("{context}.fields.{path}.storage"), field.storage())?;
        let codec = field.codec();
        if codec == "propertyText" {
            if !truthy(field.storage_setting("metadataContainer"))
                && !truthy(field.storage_setting("metadataContainerTemplate"))
            {
                return refuse(format!(
                    "{context}.fields.{path}.storage requires metadataContainer or metadataContainerTemplate for propertyText."
                ));
            }
            if !truthy(field.storage_setting("metadataKey")) {
                return refuse(format!(
                    "{context}.fields.{path}.storage.metadataKey is required for propertyText."
                ));
            }
        }
        if codec == "structuredJson" && field.value().get("schema").is_none() {
            return refuse(format!(
                "{context}.fields.{path}.value.schema is required for structuredJson."
            ));
        }
        if let Some(schema) = field.value_schema() {
            schema_ref_name(
                definition,
                schema,
                &format!("{context}.fields.{path}.value.schema"),
            )?;
        }
        let (_, field_schema) = collaboration_schema_at(definition, entity_schema, path)?
            .expect("the path resolved above");
        // A field holding an object stores it whole: as structured JSON of its
        // schema, or, for a closed object, as property text.
        if has_type(field_schema, "object") {
            let structured = match field.value_schema() {
                Some(value_schema) if field.codec() == "structuredJson" => {
                    std::ptr::eq(dereference(definition, value_schema)?, field_schema)
                }
                _ => false,
            };
            let open = other_members_schema(field_schema).is_some()
                || record_values_schema(field_schema).is_some();
            if !structured && (open || field.codec() != "propertyText") {
                return refuse(format!(
                    "{context}.fields.{path} holds an object, so its codec is structuredJson over that object's schema."
                ));
            }
        }
    }

    check_keyed_sequence_containers(&context, &fields)?;

    // A field whose codec stores its value whole covers every path under it.
    let missing: IndexSet<&str> = required_paths
        .iter()
        .map(String::as_str)
        .filter(|required| {
            let declared = fields.iter().any(|field| field.path == *required);
            let covered = fields.iter().any(|field| {
                required.starts_with(&format!("{}.", field.path))
                    && matches!(field.codec(), "propertyText" | "structuredJson")
            });
            !declared && !covered
        })
        .collect();
    if !missing.is_empty() {
        let uncovered = shallowest_uncovered(&required_paths, &missing);
        return refuse(format!(
            "{context}.fields does not cover entity schema paths: {}.",
            uncovered.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(())
}

/// A container inside a keyed sequence holds one item's data, so its name
/// carries the identity of the item of every sequence that encloses it, and a
/// sequence's item containers carry the identity of its own items too.
/// Without one, items of different parents share a container.
fn check_keyed_sequence_containers(context: &str, fields: &[CollaborationField]) -> Result<()> {
    let sequences: Vec<&CollaborationField> = fields
        .iter()
        .filter(|field| field.storage_kind() == "keyedSequence")
        .collect();
    for field in fields {
        let enclosing: Vec<&str> = sequences
            .iter()
            .filter(|sequence| field.path.starts_with(&format!("{}.*.", sequence.path)))
            .map(|sequence| sequence_identity(sequence))
            .collect();
        for key in [
            "container",
            "containerTemplate",
            "metadataContainer",
            "metadataContainerTemplate",
            "orderContainer",
            "itemContainerTemplate",
        ] {
            let Some(template) = field.storage_setting(key) else {
                continue;
            };
            let own = (key == "itemContainerTemplate").then(|| sequence_identity(field));
            for variable in enclosing.iter().copied().chain(own) {
                if !template.contains(&format!("{{{variable}}}")) {
                    return refuse(format!(
                        "{context}.fields.{}.storage.{key} must name {{{variable}}}: each item of its keyed sequence needs a container of its own.",
                        field.path
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The variable a keyed sequence's item identity is named by in templates.
fn sequence_identity<'a>(sequence: &CollaborationField<'a>) -> &'a str {
    sequence
        .storage_setting("identityVariable")
        .or(sequence.storage_setting("identityPath"))
        .expect("a keyed sequence names its identity")
}

fn check_collaboration_storage(context: &str, storage: &Object) -> Result<()> {
    let kind = storage.str_field("kind");
    let present = |key: &str| truthy(storage.opt_str(key));
    let require = |key: &str| -> Result<()> {
        if present(key) {
            Ok(())
        } else {
            refuse(format!("{context}.{key} is required for {kind}."))
        }
    };
    match kind {
        "scalar" => {
            if !present("container") && !present("containerTemplate") {
                return refuse(format!(
                    "{context} requires container or containerTemplate for scalar."
                ));
            }
            require("key")
        }
        "text" | "orderedList" | "structuredList" | "structuredMap" | "structuredDocument" => {
            if !present("container") && !present("containerTemplate") {
                return refuse(format!(
                    "{context} requires container or containerTemplate for {kind}."
                ));
            }
            Ok(())
        }
        "keyedSequence" => {
            require("identityPath")?;
            require("orderContainer")?;
            require("itemContainerTemplate")
        }
        "derivedIdentity" | "derivedRevision" => Ok(()),
        other => unreachable!("the meta-schema admits no storage kind {other:?}"),
    }
}

/// `schema`, or the schema at the end of its chain of references.
pub(crate) fn dereference<'a>(
    definition: &'a Definition,
    schema: &'a Object,
) -> Result<&'a Object> {
    let mut schema = schema;
    while matches!(schema.get("$ref"), Some(Json::String(_))) {
        schema = resolve_ref(definition, schema, "collaboration schema")?;
    }
    Ok(schema)
}

/// How the entity schema requires the value at a collaboration path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Presence {
    /// Every segment of the path is required.
    pub required: bool,
    /// The object holding the value requires it.
    pub required_in_parent: bool,
}

/// How the schema of `entity` requires the value at `path`, or `None` when
/// the path does not resolve in it.
pub(crate) fn collaboration_path_presence(
    definition: &Definition,
    entity: &str,
    path: &str,
) -> Result<Option<Presence>> {
    let schema = definition
        .entity(entity)
        .expect("validation requires the collaboration entity to exist")
        .schema();
    let root = resolve_ref(definition, schema, "collaboration schema")?;
    resolve_collaboration_schema_path(definition, root, path)
}

/// The members of an object schema that collaboration field paths address.
/// An open object's members are one value, which a field stores whole.
pub(crate) fn addressable_members(schema: &Object) -> Option<&Object> {
    if !has_type(schema, "object") || other_members_schema(schema).is_some() {
        return None;
    }
    object_at(schema, "properties")
}

/// Whether a collaboration field path resolves in the entity schema, and if
/// so how the schema requires its value.
fn resolve_collaboration_schema_path(
    definition: &Definition,
    root: &Object,
    field_path: &str,
) -> Result<Option<Presence>> {
    Ok(collaboration_schema_at(definition, root, field_path)?.map(|(presence, _)| presence))
}

/// How the schema `root` requires the value at a collaboration path, and that
/// value's schema, or `None` when the path does not resolve in it.
fn collaboration_schema_at<'a>(
    definition: &'a Definition,
    root: &'a Object,
    field_path: &str,
) -> Result<Option<(Presence, &'a Object)>> {
    let mut schema = dereference(definition, root)?;
    let mut required = true;
    let mut required_in_parent = true;
    for segment in field_path.split('.') {
        schema = dereference(definition, schema)?;
        if segment == "*" {
            let items = has_type(schema, "array")
                .then(|| object_at(schema, "items"))
                .flatten();
            let Some(items) = items else {
                return Ok(None);
            };
            schema = dereference(definition, items)?;
            continue;
        }
        let property =
            addressable_members(schema).and_then(|properties| object_at(properties, segment));
        let Some(property) = property else {
            return Ok(None);
        };
        required_in_parent =
            read_string_array(schema.get("required"), "collaboration schema required")?
                .contains(&segment);
        required = required && required_in_parent;
        schema = dereference(definition, property)?;
    }
    Ok(Some((
        Presence {
            required,
            required_in_parent,
        },
        schema,
    )))
}

/// Every path under the entity schema a collaboration field must cover, with
/// array items as `*`.
fn collect_collaboration_schema_paths(
    definition: &Definition,
    raw_schema: &Object,
    prefix: &str,
) -> Result<IndexSet<String>> {
    let schema = dereference(definition, raw_schema)?;
    if let Some(items) = has_type(schema, "array")
        .then(|| object_at(schema, "items"))
        .flatten()
    {
        let item = dereference(definition, items)?;
        let mut paths = IndexSet::from([prefix.to_owned()]);
        if has_type(item, "object") {
            paths.extend(collect_collaboration_schema_paths(
                definition,
                item,
                &format!("{prefix}.*"),
            )?);
        }
        return Ok(paths);
    }
    if let Some(properties) = addressable_members(schema) {
        let mut paths = IndexSet::new();
        for (property, property_schema) in properties.iter() {
            let Some(property_schema) = property_schema.as_object() else {
                continue;
            };
            let path = if prefix.is_empty() {
                property.to_owned()
            } else {
                format!("{prefix}.{property}")
            };
            paths.extend(collect_collaboration_schema_paths(
                definition,
                property_schema,
                &path,
            )?);
        }
        return Ok(paths);
    }
    Ok(IndexSet::from([prefix.to_owned()]))
}

/// Each uncovered path as the shallowest path under which nothing is covered,
/// so an object no field covers is named once rather than by its leaves.
fn shallowest_uncovered<'a>(
    required: &'a IndexSet<String>,
    missing: &IndexSet<&'a str>,
) -> IndexSet<&'a str> {
    missing
        .iter()
        .map(|path| {
            path.match_indices('.')
                .map(|(end, _)| &path[..end])
                .filter(|prefix| !prefix.ends_with('*'))
                .find(|prefix| {
                    let under = format!("{prefix}.");
                    required
                        .iter()
                        .filter(|required| required == prefix || required.starts_with(&under))
                        .all(|required| missing.contains(required.as_str()))
                })
                .unwrap_or(path)
        })
        .collect()
}

/// Generated identifiers from independent concepts can still collide after
/// normalisation, especially Rust enum helpers.
fn check_generated_output_names(definition: &Definition) -> Result<()> {
    let mut output_names: HashMap<String, String> = HashMap::new();
    let mut register = |name: String, source: String| -> Result<()> {
        if let Some(previous) = output_names.get(&name) {
            return refuse(format!(
                "{source} and {previous} both generate output name {name}."
            ));
        }
        output_names.insert(name, source);
        Ok(())
    };

    // The generator's own shared declarations are Clerkenwell's types, which
    // the bindings import rather than declare.
    for (definition_name, _) in definition.declared_def_entries() {
        register(
            pascal_identifier(definition_name),
            format!("$defs.{definition_name}"),
        )?;
    }
    for property_enum in collect_rust_property_enums(definition)
        .into_iter()
        .filter(|property_enum| !crate::expand::SHARED.contains(&property_enum.def.as_str()))
    {
        register(property_enum.name, property_enum.source)?;
    }
    let mut imported: IndexSet<&str> = IndexSet::new();
    imported.extend(crate::typescript::client_bindings());
    imported.extend(crate::rust::import_bindings());
    for name in imported {
        register(
            name.to_owned(),
            format!("the generated bindings' import of {name}"),
        )?;
    }
    register(
        "CollaborationDocument".to_owned(),
        "the generated collaborative document types".to_owned(),
    )?;
    register(
        "ProjectionName".to_owned(),
        "generated projection dispatch enum".to_owned(),
    )?;
    register(
        "MutationName".to_owned(),
        "generated mutation dispatch enum".to_owned(),
    )?;
    register(
        "CommandName".to_owned(),
        "generated command dispatch enum".to_owned(),
    )?;
    register(
        "ENTITY_NAMES".to_owned(),
        "generated entity name list".to_owned(),
    )?;
    register(
        "PROJECTION_NAMES".to_owned(),
        "generated projection name list".to_owned(),
    )?;
    register(
        "MUTATION_NAMES".to_owned(),
        "generated mutation name list".to_owned(),
    )?;
    register(
        "COMMAND_NAMES".to_owned(),
        "generated command name list".to_owned(),
    )?;
    for entity in definition.entities() {
        register(
            name_constant_identifier(entity.name, "ENTITY"),
            format!("entities.{}", entity.name),
        )?;
    }
    for projection in definition.projections() {
        let source = format!("projections.{}", projection.name);
        register(
            name_constant_identifier(projection.name, "PROJECTION"),
            source.clone(),
        )?;
        register(pascal_identifier(projection.name), source)?;
    }
    for mutation in definition.mutations() {
        let source = format!("mutations.{}", mutation.name);
        register(
            name_constant_identifier(mutation.name, "MUTATION"),
            source.clone(),
        )?;
        register(pascal_identifier(mutation.name), source)?;
    }
    for command in definition.commands() {
        let source = format!("commands.{}", command.name);
        register(
            name_constant_identifier(command.name, "COMMAND"),
            source.clone(),
        )?;
        register(pascal_identifier(command.name), source)?;
    }
    Ok(())
}

/// An entity id or revision field must be a declared property of the entity schema.
fn check_entity_schema_property(schema: &Object, property: &str, context: &str) -> Result<()> {
    let declared = has_type(schema, "object")
        && object_at(schema, "properties")
            .is_some_and(|properties| properties.contains_key(property));
    if declared {
        return Ok(());
    }
    refuse(format!(
        "{context} references property {} that is not declared on the entity schema.",
        string_literal(property)
    ))
}

/// Keywords that shape a generated type. Each has an exact TypeScript and Rust
/// rendering; a construct outside them fails instead of widening to `unknown`.
const TYPE_SCHEMA_KEYS: &[&str] = &[
    "$ref",
    "additionalProperties",
    "description",
    "enum",
    "items",
    "properties",
    "required",
    "title",
    "type",
    // An internally tagged union: `discriminator` names the property carrying
    // the tag, `oneOf` maps each tag value to that variant's object schema.
    "discriminator",
    "oneOf",
];

/// Keywords that constrain a value without changing its type. The type
/// renderers ignore them; they ride into the schema-shaped artefacts.
pub(crate) const VALIDATION_SCHEMA_KEYS: &[&str] = &["const", "minLength", "minimum", "maximum"];

/// Informational: what the owner supplies when the field is absent.
const ANNOTATION_SCHEMA_KEYS: &[&str] = &["default"];

/// Every keyword a `$defs` schema node may use.
pub fn schema_keywords() -> impl Iterator<Item = &'static str> {
    [
        TYPE_SCHEMA_KEYS,
        VALIDATION_SCHEMA_KEYS,
        ANNOTATION_SCHEMA_KEYS,
    ]
    .into_iter()
    .flatten()
    .copied()
}

fn is_allowed_schema_key(key: &str) -> bool {
    schema_keywords().any(|keyword| keyword == key)
}

/// Refuses a schema node outside the subset every renderer can represent exactly.
fn check_schema_node(definition: &Definition, schema: Option<&Json>, context: &str) -> Result<()> {
    let Some(Json::Object(schema)) = schema else {
        return refuse(format!("{context} must be an object schema."));
    };
    for key in schema.keys() {
        if !is_allowed_schema_key(key) {
            return refuse(format!(
                "{context} uses unsupported schema keyword {}.",
                string_literal(key)
            ));
        }
    }

    // References may carry annotations; validation constraints belong to their definition.
    if schema.get("$ref").is_some() {
        if schema
            .keys()
            .any(|key| !["$ref", "default", "description", "title"].contains(&key))
        {
            return refuse(format!(
                "{context} may not combine $ref with validation keywords."
            ));
        }
        schema_ref_name(definition, schema, context)?;
        if let Some(default) = schema.get("default") {
            let mut referenced = dereference(definition, schema)?.clone();
            referenced.insert("default", default.clone());
            check_default_value(&referenced, context)?;
        }
        return Ok(());
    }

    if let Some(values) = schema.get("enum") {
        let string_enum = values.as_array().is_some_and(|values| {
            !values.is_empty() && values.iter().all(|value| value.as_str().is_some())
        });
        if !string_enum {
            return refuse(format!("{context}.enum must be a non-empty string enum."));
        }
        if !has_type(schema, "string") {
            return refuse(format!(
                "{context}.enum is supported only on string schemas."
            ));
        }
        let mut variants: HashMap<String, &str> = HashMap::new();
        for value in values
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
        {
            let variant = pascal_identifier(value);
            if let Some(previous) = variants.insert(variant.clone(), value) {
                return refuse(format!(
                    "{context}.enum values {} and {} both generate the Rust variant {variant}.",
                    string_literal(previous),
                    string_literal(value)
                ));
            }
        }
    }

    if schema.get("oneOf").is_some() || schema.get("discriminator").is_some() {
        return check_tagged_union(definition, schema, context);
    }

    if let Some(members) = primitive_union_types(schema)? {
        check_applicable_keywords(schema, &members, context)?;
        check_validation_keywords(schema, &members, context)?;
        return check_default_value(schema, context);
    }

    let Some(schema_type) = schema.get("type").and_then(Json::as_str) else {
        return refuse(format!("{context} must declare a string type."));
    };
    check_applicable_keywords(schema, &[schema_type], context)?;

    check_validation_keywords(schema, &[schema_type], context)?;
    check_default_value(schema, context)?;

    match schema_type {
        "object" => {
            let additional = schema.get("additionalProperties");
            let other_members = additional.and_then(Json::as_object);
            if other_members.is_none() && additional != Some(&Json::Bool(false)) {
                return refuse(format!(
                    "{context} object schemas must declare additionalProperties: false or a typed schema."
                ));
            }
            if let Some(other_members) = other_members {
                check_schema_node(
                    definition,
                    additional,
                    &format!("{context}.additionalProperties"),
                )?;
                if schema.get("properties").is_none() {
                    if schema.get("required").is_some() {
                        return refuse(format!(
                            "{context}.required may name only members its properties declare."
                        ));
                    }
                    return Ok(());
                }
                check_open_object(definition, schema, other_members, context)?;
            }
            let Some(properties) = object_at(schema, "properties") else {
                return refuse(format!("{context} object schemas must declare properties."));
            };
            for required in
                read_string_array(schema.get("required"), &format!("{context}.required"))?
            {
                if !properties.contains_key(required) {
                    return refuse(format!(
                        "{context}.required references unknown property {}.",
                        string_literal(required)
                    ));
                }
            }
            for (property_name, property_schema) in properties.iter() {
                check_schema_node(
                    definition,
                    Some(property_schema),
                    &format!("{context}.properties.{property_name}"),
                )?;
            }
            Ok(())
        }
        "array" => {
            // One renderable item schema; tuples and heterogeneous arrays are
            // outside the subset.
            let Some(items) = schema
                .get("items")
                .filter(|items| items.as_object().is_some())
            else {
                return refuse(format!(
                    "{context} array schemas must declare object-valued items."
                ));
            };
            check_schema_node(definition, Some(items), &format!("{context}.items"))
        }
        "string" | "boolean" | "integer" | "number" | "json" => Ok(()),
        other => refuse(format!(
            "{context} uses unsupported schema type {}.",
            string_literal(other)
        )),
    }
}

/// Refuses a `$defs` entry whose chain of references returns to an entry
/// already on it, so following references always ends at a schema.
fn check_reference_chains(definition: &Definition) -> Result<()> {
    for (definition_name, schema) in definition.defs().iter() {
        let mut chain = vec![definition_name.to_owned()];
        let mut current = schema.as_object();
        while let Some(reference) = current.filter(|schema| schema.get("$ref").is_some()) {
            let context = format!("$defs.{}", chain.last().expect("the chain starts named"));
            let name = schema_ref_name(definition, reference, &context)?;
            if chain.contains(&name) {
                chain.push(name);
                return refuse(format!(
                    "$defs.{definition_name} is a cycle of references: {}.",
                    chain.join(" -> ")
                ));
            }
            current = definition.def(&name);
            chain.push(name);
        }
    }
    Ok(())
}

/// Refuses a structural keyword on a schema none of whose `types` it shapes.
fn check_applicable_keywords(schema: &Object, types: &[&str], context: &str) -> Result<()> {
    for (keyword, shaped) in [
        ("properties", "object"),
        ("additionalProperties", "object"),
        ("required", "object"),
        ("items", "array"),
    ] {
        if schema.get(keyword).is_some() && !types.contains(&shaped) {
            return refuse(format!(
                "{context}.{keyword} applies only to {shaped} schemas."
            ));
        }
    }
    Ok(())
}

/// An open object declares at least one member, each a value of its other
/// members' schema, and none named as the Rust field holding the others.
fn check_open_object(
    definition: &Definition,
    schema: &Object,
    other_members: &Object,
    context: &str,
) -> Result<()> {
    let Some(properties) =
        object_at(schema, "properties").filter(|properties| !properties.is_empty())
    else {
        return refuse(format!(
            "{context} declares no properties beside additionalProperties; a record omits properties."
        ));
    };
    for (property_name, property_schema) in properties.iter() {
        let context = format!("{context}.properties.{property_name}");
        if crate::names::rust_field_identifier(property_name) == crate::rust::OTHER_MEMBERS_FIELD {
            return refuse(format!(
                "{context} is named as the Rust field holding the other members."
            ));
        }
        let Some(property_schema) = property_schema.as_object() else {
            return refuse(format!("{context} must be an object schema."));
        };
        if !admits(
            definition,
            other_members,
            property_schema,
            &mut HashSet::new(),
        )? {
            return refuse(format!(
                "{context} is not a value of the object's additionalProperties schema."
            ));
        }
    }
    Ok(())
}

/// Whether every value of `member` is a value of `values`. A pair of schemas
/// the subset cannot compare is not admitted. `assumed` holds the reference
/// pairs being compared, so recursive definitions compare coinductively.
fn admits(
    definition: &Definition,
    values: &Object,
    member: &Object,
    assumed: &mut HashSet<(String, String)>,
) -> Result<bool> {
    if has_type(values, "json") {
        return Ok(true);
    }
    let values_ref = values.get("$ref").and_then(Json::as_str);
    let member_ref = member.get("$ref").and_then(Json::as_str);
    if values_ref.is_some() && values_ref == member_ref {
        return Ok(true);
    }
    if values_ref.is_some() || member_ref.is_some() {
        let pair = (
            Json::from(values.clone()).stringify(),
            Json::from(member.clone()).stringify(),
        );
        if !assumed.insert(pair) {
            return Ok(true);
        }
        let values = dereference(definition, values)?;
        let member = dereference(definition, member)?;
        // References ending at one definition name one type.
        if std::ptr::eq(values, member) {
            return Ok(true);
        }
        return admits(definition, values, member, assumed);
    }

    let member_types: Vec<&str> = match primitive_union_types(member)? {
        Some(types) => types,
        None => match member.get("type").and_then(Json::as_str) {
            Some(member_type) => vec![member_type],
            None => return Ok(false),
        },
    };
    let values_types: Vec<&str> = match primitive_union_types(values)? {
        Some(types) => types,
        None => match values.get("type").and_then(Json::as_str) {
            Some(values_type) => vec![values_type],
            None => return Ok(false),
        },
    };
    // The values of a member that enumerates them, less those its own bounds
    // exclude, are checked one by one.
    let member_values: Option<Vec<&Json>> = match (member.get("enum"), member.get("const")) {
        (Some(Json::Array(values)), _) => Some(
            values
                .iter()
                .filter(|value| value_meets_bounds(member, value))
                .collect(),
        ),
        (_, Some(constant)) => Some(vec![constant]),
        _ => None,
    };
    // A member that lists its values is admitted by their types; any other,
    // by its declared types.
    let scalar_admitted = |member_type: &str| {
        values_types.iter().any(|values_type| {
            *values_type == member_type || (*values_type == "number" && member_type == "integer")
        })
    };
    let typed = match &member_values {
        Some(member_values) => member_values.iter().all(|value| {
            values_types.iter().any(|values_type| match *values_type {
                "integer" => is_integer(value),
                "number" => value.as_f64().is_some(),
                "string" => value.as_str().is_some(),
                "boolean" => value.as_bool().is_some(),
                _ => false,
            })
        }),
        None => member_types
            .iter()
            .all(|member_type| scalar_admitted(member_type)),
    };
    if !typed {
        return Ok(false);
    }
    // Both bindings render an enumerated other member as its values, and a
    // member as its own enum's values or, without one, its type; so each
    // value of the member's enum, its own bounds aside, is one of them.
    if let Some(admitted) = values.get("enum").and_then(Json::as_array) {
        let Some(listed) = member.get("enum").and_then(Json::as_array) else {
            return Ok(false);
        };
        if !listed.iter().all(|value| admitted.contains(value)) {
            return Ok(false);
        }
    }
    if let Some(constant) = values.get("const") {
        let Some(member_values) = &member_values else {
            return Ok(false);
        };
        if !member_values.iter().all(|value| *value == constant) {
            return Ok(false);
        }
    }
    if let Some(member_values) = &member_values {
        if !member_values
            .iter()
            .all(|value| value_meets_bounds(values, value))
        {
            return Ok(false);
        }
    } else {
        // A bound constrains only the values of its own type, so it binds a
        // member whose values include that type.
        let numeric = member_types
            .iter()
            .any(|member_type| matches!(*member_type, "integer" | "number"));
        let textual = member_types.contains(&"string");
        for (bound, tighter, applies) in [
            ("minimum", true, numeric),
            ("maximum", false, numeric),
            ("minLength", true, textual),
        ] {
            let Some(limit) = values.get(bound).and_then(Json::as_f64).filter(|_| applies) else {
                continue;
            };
            // A numeric bound of a schema whose numbers are integers is the
            // integer it leaves; its other types are not bounded.
            let integral = |types: &[&str], value: f64| {
                let integers = !types.contains(&"number") && types.contains(&"integer");
                if bound == "minLength" || !integers {
                    value
                } else if tighter {
                    value.ceil()
                } else {
                    value.floor()
                }
            };
            let limit = integral(&values_types, limit);
            let held = member
                .get(bound)
                .and_then(Json::as_f64)
                .map(|value| integral(&member_types, value))
                .is_some_and(|value| {
                    if tighter {
                        value >= limit
                    } else {
                        value <= limit
                    }
                });
            if !held {
                return Ok(false);
            }
        }
    }
    if values_types.len() > 1 || member_types.len() > 1 {
        return Ok(true);
    }

    match values_types[0] {
        "array" => {
            let (Some(values_items), Some(member_items)) =
                (object_at(values, "items"), object_at(member, "items"))
            else {
                return Ok(false);
            };
            admits(definition, values_items, member_items, assumed)
        }
        "object" => {
            // Records admit records of admitted values; other objects admit
            // only themselves, by reference.
            match (record_values_schema(values), record_values_schema(member)) {
                (Some(values), Some(member)) => admits(definition, values, member, assumed),
                _ => Ok(false),
            }
        }
        _ => Ok(true),
    }
}

fn is_integer(value: &Json) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number.trunc() == number)
}

/// Checks the validation-only keywords against the types they constrain.
fn check_validation_keywords(schema: &Object, types: &[&str], context: &str) -> Result<()> {
    let numeric = types
        .iter()
        .any(|member| matches!(*member, "integer" | "number"));
    for name in ["minimum", "maximum"] {
        if let Some(bound) = schema.get(name) {
            if !numeric || !bound.as_f64().is_some_and(f64::is_finite) {
                return refuse(format!(
                    "{context}.{name} requires a finite numeric bound on a numeric schema."
                ));
            }
        }
    }
    if let (Some(min), Some(max)) = (
        schema.get("minimum").and_then(Json::as_f64),
        schema.get("maximum").and_then(Json::as_f64),
    ) {
        if min > max {
            return refuse(format!("{context}.minimum exceeds maximum."));
        }
    }

    if let Some(constant) = schema.get("const") {
        if schema.get("enum").is_some() {
            return refuse(format!("{context} may not declare both const and enum."));
        }
        let fits = |member: &str| match member {
            "integer" => is_integer(constant),
            "number" => constant.as_f64().is_some(),
            "string" => constant.as_str().is_some(),
            "boolean" => constant.as_bool().is_some(),
            _ => false,
        };
        match types {
            [single] => {
                let expected = match *single {
                    "integer" | "number" => "number",
                    "string" => "string",
                    "boolean" => "boolean",
                    _ => {
                        return refuse(format!(
                            "{context}.const is supported only on scalar schemas."
                        ))
                    }
                };
                if !fits(expected) {
                    return refuse(format!(
                        "{context}.const must be a {expected} to match its declared type."
                    ));
                }
                if !fits(single) {
                    return refuse(format!(
                        "{context}.const must be an integer to match its declared type."
                    ));
                }
            }
            _ => {
                if !types.iter().any(|member| fits(member)) {
                    return refuse(format!(
                        "{context}.const must be a value of one of its types."
                    ));
                }
            }
        }
        // A numeric constant only an integer member holds is an `i64`, as the
        // Rust model's integers are.
        let representable = constant
            .as_f64()
            .is_none_or(|value| value >= i64::MIN as f64 && value < -(i64::MIN as f64));
        if types.contains(&"integer") && !types.contains(&"number") && !representable {
            return refuse(format!("{context}.const must be an integer an i64 holds."));
        }
    }

    if let Some(constant) = schema.get("const") {
        for bound in ["minLength", "minimum", "maximum"] {
            let mut bounded = Object::new();
            bounded.insert_some(bound, schema.get(bound).cloned());
            if !value_meets_bounds(&bounded, constant) {
                return refuse(format!("{context}.const does not meet its {bound}."));
            }
        }
    }

    if let Some(min_length) = schema.get("minLength") {
        if !types.contains(&"string") {
            return refuse(format!(
                "{context}.minLength is supported only on string schemas."
            ));
        }
        if !is_integer(min_length) || min_length.as_f64().is_some_and(|value| value < 0.0) {
            return refuse(format!(
                "{context}.minLength must be a non-negative integer."
            ));
        }
    }
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        if !values.iter().any(|value| value_meets_bounds(schema, value)) {
            return refuse(format!(
                "{context}.enum has no value that meets its bounds."
            ));
        }
    }
    Ok(())
}

/// A tagged union is a named type, declared in `$defs` and reached by `$ref`.
fn check_tagged_union(definition: &Definition, schema: &Object, context: &str) -> Result<()> {
    let top_level = context
        .strip_prefix("$defs.")
        .is_some_and(|name| !name.contains('.'));
    if !top_level {
        return refuse(format!(
            "{context} may only declare a tagged union at the top level of a $defs entry."
        ));
    }
    if !has_type(schema, "object") {
        return refuse(format!(
            "{context} must declare type \"object\" alongside its oneOf."
        ));
    }
    let discriminator = schema
        .get("discriminator")
        .and_then(Json::as_str)
        .filter(|discriminator| !crate::names::js_trim(discriminator).is_empty());
    let Some(discriminator) = discriminator else {
        return refuse(format!(
            "{context}.discriminator must name the property carrying the tag."
        ));
    };
    if schema.get("properties").is_some() || schema.get("additionalProperties").is_some() {
        return refuse(format!(
            "{context} may not declare properties beside its oneOf; each variant declares its own."
        ));
    }
    let Some(variants) = object_at(schema, "oneOf") else {
        return refuse(format!(
            "{context}.oneOf must map each tag value to that variant's schema."
        ));
    };
    if variants.is_empty() {
        return refuse(format!(
            "{context}.oneOf must declare at least one variant."
        ));
    }
    for (tag, variant) in variants.iter() {
        if crate::names::js_trim(tag).is_empty() {
            return refuse(format!("{context}.oneOf declares an empty tag value."));
        }
        let Some(variant_schema) = variant.as_object() else {
            return refuse(format!("{context}.oneOf.{tag} must be an object schema."));
        };
        if !has_type(variant_schema, "object") {
            return refuse(format!(
                "{context}.oneOf.{tag} must declare type \"object\"."
            ));
        }
        // serde writes the tag from the variant it picked; a variant that also
        // declared it would round-trip a second, unrelated field.
        if object_at(variant_schema, "properties")
            .is_some_and(|properties| properties.contains_key(discriminator))
        {
            return refuse(format!(
                "{context}.oneOf.{tag} declares {}, which the tag already carries.",
                string_literal(discriminator)
            ));
        }
        if variant_schema
            .get("additionalProperties")
            .is_some_and(|additional| additional.as_object().is_some())
        {
            return refuse(format!(
                "{context}.oneOf.{tag} must be a closed object: additionalProperties false."
            ));
        }
        check_schema_node(definition, Some(variant), &format!("{context}.oneOf.{tag}"))?;
    }
    Ok(())
}

fn check_default_value(schema: &Object, context: &str) -> Result<()> {
    let Some(default) = schema.get("default") else {
        return Ok(());
    };
    if let Some(members) = primitive_union_types(schema)? {
        let matches = members.iter().any(|member| match *member {
            "string" => default.as_str().is_some(),
            "boolean" => default.as_bool().is_some(),
            "integer" => is_integer(default),
            _ => default.as_f64().is_some(),
        });
        if !matches {
            return refuse(format!(
                "{context}.default must match one of {}.",
                Json::from(members).stringify()
            ));
        }
        return Ok(());
    }
    let schema_type = schema.get("type");
    let matches_type = match schema_type.and_then(Json::as_str) {
        Some("array") => default.as_array().is_some(),
        Some("string") => default.as_str().is_some(),
        Some("boolean") => default.as_bool().is_some(),
        Some("integer") => is_integer(default),
        Some("number") => default.as_f64().is_some(),
        Some("object") => default.as_object().is_some(),
        _ => false,
    };
    if !matches_type {
        return refuse(format!(
            "{context}.default must be a {} to match its declared type.",
            crate::schema::stringify_or_undefined(schema_type)
        ));
    }
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        if !values.contains(default) {
            return refuse(format!(
                "{context}.default must be one of the values its enum admits."
            ));
        }
    }
    Ok(())
}
