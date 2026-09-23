//! Definition validation: the meta-schema, then the invariants JSON Schema
//! cannot state, each refusal naming the definition path at fault.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use indexmap::IndexSet;

use crate::collate::code_unit_compare;
use crate::config::Project;
use crate::definition::{CollaborationEntity, Definition, Fields, Projection};
use crate::error::{refuse, Result};
use crate::json::{string_literal, Json, Object};
use crate::names::{constant_name, name_constant_identifier, pascal_identifier};
use crate::rust::collect_rust_property_enums;
use crate::schema::{
    has_type, object_at, primitive_union_types, read_string_array, resolve_ref, schema_ref_name,
    schema_ref_type_name,
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
            let path = error.instance_path.as_str();
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

/// Every configured name must be one the definition declares.
pub(crate) fn check_project_names(definition: &Definition, project: &Project) -> Result<()> {
    if let Some(name) = &project.authoring_session_mnemonic {
        if definition.mnemonic(name).is_none() {
            return refuse(format!(
                "The configured authoringSessionMnemonic {} is not declared in mnemonic.",
                string_literal(name)
            ));
        }
    }
    for name in &project.collaboration_leaf_schemas {
        if definition.def(name).is_none() {
            return refuse(format!(
                "The configured collaborationLeafSchemas entry {} is not declared in $defs.",
                string_literal(name)
            ));
        }
    }
    for name in project.entity_diagnostics.keys() {
        if definition.entity(name).is_none() {
            return refuse(format!(
                "The configured entityDiagnostics entry {} is not declared in entities.",
                string_literal(name)
            ));
        }
    }
    Ok(())
}

/// Whether the configured authoring session mnemonic is declared.
fn has_session_mnemonic(definition: &Definition, project: &Project) -> bool {
    project
        .authoring_session_mnemonic
        .as_deref()
        .is_some_and(|name| definition.mnemonic(name).is_some())
}

pub(crate) fn check_semantics(definition: &Definition, project: &Project) -> Result<()> {
    let entity_names = definition.entity_names();
    let collaboration = definition.collaboration();

    check_version_compatibility(
        definition.version(),
        definition.compatibility(),
        "compatibility",
    )?;
    check_version_compatibility(
        collaboration.version(),
        collaboration.compatibility(),
        "collaboration.compatibility",
    )?;

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
                "entities.{name}.revision.field is not supported for loro revisions."
            ));
        }
        let kind = entity.authoring_kind();
        if kind == "collaborative" && entity.revision_kind() != "loro" {
            return refuse(format!(
                "entities.{name}.authoring.kind \"collaborative\" requires a loro revision."
            ));
        }
        if entity.owns_session() && !has_session_mnemonic(definition, project) {
            return refuse(match &project.authoring_session_mnemonic {
                Some(mnemonic) => format!(
                    "entities.{name}.authoring.kind {} requires mnemonic.{mnemonic}.",
                    string_literal(kind)
                ),
                None => format!(
                    "entities.{name}.authoring.kind {} requires an authoring session mnemonic, and the configuration names none (authoringSessionMnemonic).",
                    string_literal(kind)
                ),
            });
        }
        let entity_mutations: HashSet<&str> = definition
            .mutations()
            .filter(|mutation| mutation.touches().contains(&name))
            .map(|mutation| mutation.name)
            .collect();
        let content = entity.content_mutation();
        if let Some(content) = content {
            if !entity_mutations.contains(content) {
                return refuse(format!(
                    "entities.{name}.authoring.contentMutation must name a mutation touching {name}."
                ));
            }
        }
        let lifecycle = entity.lifecycle_mutations();
        let planning = entity.planning_mutations();
        for mutation in &lifecycle {
            check_classified_mutation(&entity_mutations, name, "lifecycleMutations", mutation)?;
            if Some(*mutation) == content {
                return refuse(format!(
                    "entities.{name}.authoring cannot classify {} as both content and lifecycle.",
                    string_literal(mutation)
                ));
            }
        }
        for mutation in &planning {
            check_classified_mutation(&entity_mutations, name, "planningMutations", mutation)?;
            if Some(*mutation) == content || lifecycle.contains(mutation) {
                return refuse(classified_more_than_once(name, mutation));
            }
        }
        for mutation in entity.command_mutations() {
            check_classified_mutation(&entity_mutations, name, "commandMutations", mutation)?;
            if Some(mutation) == content
                || planning.contains(&mutation)
                || lifecycle.contains(&mutation)
            {
                return refuse(classified_more_than_once(name, mutation));
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
        if entity.revision_kind() != "loro" || entity.authoring_kind() != "collaborative" {
            return refuse(format!(
                "collaboration.entities.{name} requires a collaborative entity with a loro revision."
            ));
        }
        check_collaboration_entity(definition, project, collaboration_entity)?;
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

    for mnemonic in definition.mnemonics() {
        schema_ref_name(
            definition,
            mnemonic.schema(),
            &format!("mnemonic.{}.schema", mnemonic.name),
        )?;
    }

    check_generated_output_names(definition)?;

    // Projection and mutation names share a public API namespace.
    let mut public_names = HashSet::new();
    for name in definition
        .projections()
        .map(|projection| projection.name)
        .chain(definition.mutations().map(|mutation| mutation.name))
    {
        if !public_names.insert(name) {
            return refuse(format!(
                "Projection and mutation names must be unique; {} is duplicated.",
                string_literal(name)
            ));
        }
    }
    Ok(())
}

fn check_classified_mutation(
    entity_mutations: &HashSet<&str>,
    entity: &str,
    list: &str,
    mutation: &str,
) -> Result<()> {
    if entity_mutations.contains(mutation) {
        return Ok(());
    }
    refuse(format!(
        "entities.{entity}.authoring.{list} references mutation {} that does not touch {entity}.",
        string_literal(mutation)
    ))
}

fn classified_more_than_once(entity: &str, mutation: &str) -> String {
    format!(
        "entities.{entity}.authoring cannot classify {} more than once.",
        string_literal(mutation)
    )
}

fn check_version_compatibility(version: f64, compatibility: &Object, context: &str) -> Result<()> {
    let version_text = crate::json::number_to_string(version);
    if compatibility.number_field("minimumReaderVersion") > version {
        return refuse(format!(
            "{context}.minimumReaderVersion cannot exceed version {version_text}."
        ));
    }
    if compatibility.number_field("minimumWriterVersion") > version {
        return refuse(format!(
            "{context}.minimumWriterVersion cannot exceed version {version_text}."
        ));
    }
    Ok(())
}

/// The patch kinds each materialisation strategy consumes.
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
    project: &Project,
    collaboration: CollaborationEntity,
) -> Result<()> {
    let entity_name = collaboration.name;
    let context = format!("collaboration.entities.{entity_name}");
    if collaboration.migration_ids().is_empty() {
        return refuse(format!(
            "{context}.migrationIds must declare the current layout migration."
        ));
    }
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
    if entity.content_mutation() != Some(import_mutation) {
        return refuse(format!(
            "{context}.authoringState.importMutation must match entities.{entity_name}.authoring.contentMutation."
        ));
    }

    let schema_name = schema_ref_name(
        definition,
        entity.schema(),
        &format!("entities.{entity_name}.schema"),
    )?;
    let entity_schema = definition.def(&schema_name).expect("resolved above");
    let required_paths =
        collect_collaboration_schema_paths(definition, project, entity_schema, "")?;
    let fields: Vec<_> = collaboration.fields().collect();

    for field in &fields {
        let path = field.path;
        let Some(resolved_required) =
            resolve_collaboration_schema_path(definition, entity_schema, path)?
        else {
            return refuse(format!(
                "{context}.fields.{path} does not resolve in {schema_name}."
            ));
        };
        if resolved_required != field.required() {
            return refuse(format!(
                "{context}.fields.{path}.required must match the entity schema ({resolved_required})."
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
    }

    let missing: Vec<&str> = required_paths
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
        return refuse(format!(
            "{context}.fields does not cover entity schema paths: {}.",
            missing.join(", ")
        ));
    }
    Ok(())
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

/// `schema`, or the definition it references.
pub(crate) fn dereference<'a>(
    definition: &'a Definition,
    schema: &'a Object,
) -> Result<&'a Object> {
    if !matches!(schema.get("$ref"), Some(Json::String(_))) {
        return Ok(schema);
    }
    resolve_ref(definition, schema, "collaboration schema")
}

/// Whether a collaboration field path resolves in the entity schema, and if
/// so whether every segment of it is required.
fn resolve_collaboration_schema_path(
    definition: &Definition,
    root: &Object,
    field_path: &str,
) -> Result<Option<bool>> {
    let mut schema = dereference(definition, root)?;
    let mut required = true;
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
        let property = has_type(schema, "object")
            .then(|| object_at(schema, "properties"))
            .flatten()
            .and_then(|properties| object_at(properties, segment));
        let Some(property) = property else {
            return Ok(None);
        };
        required = required
            && read_string_array(schema.get("required"), "collaboration schema required")?
                .contains(&segment);
        schema = dereference(definition, property)?;
    }
    Ok(Some(required))
}

/// Every path under the entity schema a collaboration field must cover: array
/// items as `*`, and a configured leaf schema as one value.
fn collect_collaboration_schema_paths(
    definition: &Definition,
    project: &Project,
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
                project,
                item,
                &format!("{prefix}.*"),
            )?);
        }
        return Ok(paths);
    }
    let is_leaf = raw_schema
        .get("$ref")
        .and_then(Json::as_str)
        .is_some_and(|reference| {
            project
                .collaboration_leaf_schemas
                .iter()
                .any(|leaf| reference == format!("#/$defs/{leaf}"))
        });
    if let Some(properties) = has_type(schema, "object")
        .then(|| object_at(schema, "properties"))
        .flatten()
        .filter(|_| !is_leaf)
    {
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
                project,
                property_schema,
                &path,
            )?);
        }
        return Ok(paths);
    }
    Ok(IndexSet::from([prefix.to_owned()]))
}

/// Generated identifiers from independent concepts can still collide after
/// normalisation, especially Rust enum helpers and mnemonic constants.
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

    for definition_name in definition.defs().keys() {
        register(
            pascal_identifier(definition_name),
            format!("$defs.{definition_name}"),
        )?;
    }
    for property_enum in collect_rust_property_enums(definition) {
        register(property_enum.name, property_enum.source)?;
    }
    register(
        "ProjectionName".to_owned(),
        "generated projection dispatch enum".to_owned(),
    )?;
    register(
        "MutationName".to_owned(),
        "generated mutation dispatch enum".to_owned(),
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
    for mnemonic in definition.mnemonics() {
        let schema_name = schema_ref_type_name(mnemonic.schema())?;
        let prefix = constant_name(mnemonic.name);
        let source = format!("mnemonic.{}", mnemonic.name);
        register(format!("{prefix}_MNEMONIC_KEY"), source.clone())?;
        register(format!("{prefix}_MNEMONIC_SCHEMA"), source.clone())?;
        register(format!("{schema_name}MnemonicValue"), source)?;
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

/// Keywords that shape a generated type. Each has an exact TypeScript, Rust and
/// mnemonic rendering; a construct outside them fails instead of widening to
/// `unknown`.
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
const VALIDATION_SCHEMA_KEYS: &[&str] = &["const", "minLength"];

/// A `json` field may stay undescribed only by saying why. Reasoned fields are
/// counted separately in the coverage report.
const DOCUMENTATION_SCHEMA_KEYS: &[&str] = &["opaqueReason"];

/// Informational: what the owner supplies when the field is absent.
const ANNOTATION_SCHEMA_KEYS: &[&str] = &["default"];

/// Every keyword a `$defs` schema node may use.
pub fn schema_keywords() -> impl Iterator<Item = &'static str> {
    [
        TYPE_SCHEMA_KEYS,
        VALIDATION_SCHEMA_KEYS,
        DOCUMENTATION_SCHEMA_KEYS,
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

    // `$ref` is all-or-nothing in this subset.
    if schema.get("$ref").is_some() {
        if schema.len() != 1 {
            return refuse(format!(
                "{context} may not combine $ref with other schema keywords."
            ));
        }
        schema_ref_name(definition, schema, context)?;
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
    }

    if schema.get("oneOf").is_some() || schema.get("discriminator").is_some() {
        return check_tagged_union(definition, schema, context);
    }

    if primitive_union_types(schema)?.is_some() {
        return check_default_value(schema, context);
    }

    let Some(schema_type) = schema.get("type").and_then(Json::as_str) else {
        return refuse(format!("{context} must declare a string type."));
    };

    check_validation_keywords(schema, context)?;
    check_default_value(schema, context)?;
    check_opaque_reason(schema, context)?;

    match schema_type {
        "object" => {
            if let Some(additional) = schema
                .get("additionalProperties")
                .filter(|value| value.as_object().is_some())
            {
                if schema.get("properties").is_some() {
                    return refuse(format!(
                        "{context} record schemas may not declare fixed properties."
                    ));
                }
                return check_schema_node(
                    definition,
                    Some(additional),
                    &format!("{context}.additionalProperties"),
                );
            }
            // Closed objects keep TypeScript, serde and mnemonic schemas aligned
            // around the same "no undeclared fields" behaviour.
            if schema.get("additionalProperties") != Some(&Json::Bool(false)) {
                return refuse(format!(
                    "{context} object schemas must declare additionalProperties: false or a typed schema."
                ));
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

fn is_integer(value: &Json) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number.trunc() == number)
}

/// Checks the validation-only keywords against the type they constrain.
fn check_validation_keywords(schema: &Object, context: &str) -> Result<()> {
    let schema_type = schema.get("type").and_then(Json::as_str);
    if let Some(constant) = schema.get("const") {
        if schema.get("enum").is_some() {
            return refuse(format!("{context} may not declare both const and enum."));
        }
        let expected = match schema_type {
            Some("integer" | "number") => "number",
            Some("string") => "string",
            Some("boolean") => "boolean",
            _ => {
                return refuse(format!(
                    "{context}.const is supported only on scalar schemas."
                ))
            }
        };
        let matches = match expected {
            "number" => constant.as_f64().is_some(),
            "string" => constant.as_str().is_some(),
            _ => constant.as_bool().is_some(),
        };
        if !matches {
            return refuse(format!(
                "{context}.const must be a {expected} to match its declared type."
            ));
        }
        if schema_type == Some("integer") && !is_integer(constant) {
            return refuse(format!(
                "{context}.const must be an integer to match its declared type."
            ));
        }
    }

    if let Some(min_length) = schema.get("minLength") {
        if schema_type != Some("string") {
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

fn check_opaque_reason(schema: &Object, context: &str) -> Result<()> {
    let Some(reason) = schema.get("opaqueReason") else {
        return Ok(());
    };
    if !has_type(schema, "json") {
        return refuse(format!(
            "{context}.opaqueReason is only meaningful on a json schema; describe this field instead."
        ));
    }
    if reason
        .as_str()
        .is_none_or(|reason| crate::names::js_trim(reason).is_empty())
    {
        return refuse(format!(
            "{context}.opaqueReason must be a non-empty string saying why arbitrary JSON is the type."
        ));
    }
    Ok(())
}
