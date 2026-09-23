//! The Rust model module: serde types for every `$defs` entry, dispatch enums,
//! and the registry and collaboration plan constants.

use crate::config::Project;
use crate::definition::{Definition, Fields, Materialization};
use crate::error::{refuse, Result};
use crate::json::{number_to_string, string_literal, Json, Object};
use crate::names::{
    constant_name, name_constant_identifier, pascal_identifier, rust_field_identifier,
    rust_property_enum_name, utf16_len,
};
use crate::schema::{
    has_type, object_at, primitive_union_types, read_string_array, ref_string,
    schema_ref_type_name, stringify_or_undefined,
};

/// A helper enum generated for a string-enum property.
pub(crate) struct RustPropertyEnum {
    pub name: String,
    pub source: String,
    pub values: Vec<String>,
}

/// Every object property with a string enum (directly or as array items), each
/// named after its definition and property so two `kind` fields stay distinct.
pub(crate) fn collect_rust_property_enums(definition: &Definition) -> Vec<RustPropertyEnum> {
    let mut enums = Vec::new();
    for (definition_name, schema) in definition.def_entries() {
        if !has_type(schema, "object") {
            continue;
        }
        let Some(properties) = object_at(schema, "properties") else {
            continue;
        };
        for (property_name, property_schema) in properties.iter() {
            let Some(property_schema) = property_schema.as_object() else {
                continue;
            };
            let enum_schema = if property_schema
                .get("enum")
                .and_then(Json::as_array)
                .is_some()
            {
                Some(property_schema)
            } else if has_type(property_schema, "array") {
                object_at(property_schema, "items")
                    .filter(|items| items.get("enum").and_then(Json::as_array).is_some())
            } else {
                None
            };
            if let Some(enum_schema) = enum_schema {
                enums.push(RustPropertyEnum {
                    name: rust_property_enum_name(definition_name, property_name),
                    source: format!("$defs.{definition_name}.properties.{property_name}"),
                    values: enum_schema
                        .get("enum")
                        .and_then(Json::as_array)
                        .expect("checked above")
                        .iter()
                        .filter_map(Json::as_str)
                        .map(str::to_owned)
                        .collect(),
                });
            }
        }
    }
    enums
}

/// The model crate's source.
pub fn render_model_module(definition: &Definition, project: &Project) -> Result<String> {
    let mut lines: Vec<String> = crate::header_lines(project)
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                "//!".to_owned()
            } else {
                format!("//! {line}")
            }
        })
        .collect();
    lines.extend(
        [
            "",
            "use serde::{Deserialize, Serialize};",
            "use serde_json::Value as JsonValue;",
            "use std::collections::HashMap;",
            "",
        ]
        .map(str::to_owned),
    );

    // TypeScript inlines literal unions; Rust names an enum for each, before
    // the structs that use them.
    for property_enum in collect_rust_property_enums(definition) {
        lines.push(render_enum(&property_enum.name, &property_enum.values));
        lines.push(String::new());
    }

    for (definition_name, schema) in definition.def_entries() {
        lines.push(render_definition(definition_name, schema)?);
        lines.push(String::new());
    }

    let entity_names = definition.entity_names();
    let projection_names: Vec<&str> = definition
        .projections()
        .map(|projection| projection.name)
        .collect();
    let mutation_names: Vec<&str> = definition
        .mutations()
        .map(|mutation| mutation.name)
        .collect();
    lines.extend(name_constants("ENTITY", &entity_names));
    lines.push(String::new());
    lines.extend(name_constants("PROJECTION", &projection_names));
    lines.push(String::new());
    lines.extend(name_constants("MUTATION", &mutation_names));
    lines.push(String::new());
    lines.push(name_enum("ProjectionName", "PROJECTION", &projection_names));
    lines.push(String::new());
    lines.push(name_enum("MutationName", "MUTATION", &mutation_names));
    lines.push(String::new());
    lines.push(string_slice_constant("ENTITY_NAMES", &entity_names));
    lines.push(string_slice_constant("PROJECTION_NAMES", &projection_names));
    lines.push(string_slice_constant("MUTATION_NAMES", &mutation_names));
    for mnemonic in definition.mnemonics() {
        lines.push(string_constant(
            &format!("{}_MNEMONIC_KEY", constant_name(mnemonic.name)),
            mnemonic.key(),
        ));
    }
    lines.push(String::new());
    let projections: Vec<_> = definition.projections().collect();
    lines.push(transport_enum(
        "ProjectionTransportSnapshot",
        "projection",
        &projections
            .iter()
            .map(|projection| {
                Ok((
                    projection.name,
                    schema_ref_type_name(projection.snapshot())?,
                ))
            })
            .collect::<Result<Vec<_>>>()?,
    ));
    lines.push(String::new());
    lines.push(transport_enum(
        "ProjectionTransportPatch",
        "projection",
        &projections
            .iter()
            .map(|projection| Ok((projection.name, schema_ref_type_name(projection.patch())?)))
            .collect::<Result<Vec<_>>>()?,
    ));
    lines.push(String::new());
    lines.push(transport_enum(
        "ProjectionTransportMutationResult",
        "mutation",
        &definition
            .mutations()
            .map(|mutation| Ok((mutation.name, schema_ref_type_name(mutation.result())?)))
            .collect::<Result<Vec<_>>>()?,
    ));
    lines.push(String::new());
    lines.push(registry_metadata(definition, project));
    lines.push(String::new());
    lines.push(collaboration_metadata(definition));
    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn transport_enum(enum_name: &str, tag: &str, entries: &[(&str, String)]) -> String {
    let mut lines = vec![
        "#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]".to_owned(),
        format!(
            "#[serde(tag = {}, content = \"value\", deny_unknown_fields)]",
            string_literal(tag)
        ),
        format!("pub enum {enum_name} {{"),
    ];
    for (name, type_name) in entries {
        lines.push(format!("    #[serde(rename = {})]", string_literal(name)));
        lines.push(format!("    {}({type_name}),", pascal_identifier(name)));
    }
    lines.push("}".to_owned());
    if tag == "mutation" {
        lines.extend(
            [
                "",
                &format!("impl {enum_name} {{"),
                "    pub const fn mutation_name(&self) -> MutationName {",
                "        match self {",
            ]
            .map(str::to_owned),
        );
        for (name, _) in entries {
            let variant = pascal_identifier(name);
            let arm = format!("            Self::{variant}(_) => MutationName::{variant},");
            if utf16_len(&arm) <= 100 {
                lines.push(arm);
            } else {
                lines.push(format!("            Self::{variant}(_) => {{"));
                lines.push(format!("                MutationName::{variant}"));
                lines.push("            }".to_owned());
            }
        }
        lines.extend(["        }", "    }", "}"].map(str::to_owned));
    }
    lines.join("\n")
}

fn mutation_list(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("MutationName::{}", pascal_identifier(name)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn string_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| string_literal(value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn registry_metadata(definition: &Definition, project: &Project) -> String {
    let mut lines: Vec<String> = REGISTRY_TYPES.lines().map(str::to_owned).collect();
    lines.push(String::new());
    lines.push("pub static GENERATED_PROJECTION_SPECS: &[GeneratedProjectionSpec] = &[".to_owned());
    for projection in definition.projections() {
        lines.push("    GeneratedProjectionSpec {".to_owned());
        lines.push(format!(
            "        name: ProjectionName::{},",
            pascal_identifier(projection.name)
        ));
        lines.push(format!(
            "        depends_on: &[{}],",
            string_list(&projection.depends_on())
        ));
        lines.extend(materialization_plan_lines(projection.materialization()));
        lines.push("    },".to_owned());
    }
    lines.extend(
        [
            "];",
            "",
            "pub static GENERATED_MUTATION_SPECS: &[GeneratedMutationSpec] = &[",
        ]
        .map(str::to_owned),
    );
    for mutation in definition.mutations() {
        lines.push("    GeneratedMutationSpec {".to_owned());
        lines.push(format!(
            "        name: MutationName::{},",
            pascal_identifier(mutation.name)
        ));
        lines.push(format!(
            "        touches: &[{}],",
            string_list(&mutation.touches())
        ));
        lines.push("    },".to_owned());
    }
    lines.extend(
        [
            "];",
            "",
            "pub static GENERATED_ENTITY_AUTHORING_SPECS: &[GeneratedEntityAuthoringSpec] = &[",
        ]
        .map(str::to_owned),
    );
    let session_key = project
        .authoring_session_mnemonic
        .as_deref()
        .and_then(|name| definition.mnemonic(name))
        .map(|mnemonic| mnemonic.key());
    for entity in definition.entities() {
        let mutations: Vec<&str> = definition
            .mutations()
            .filter(|mutation| mutation.touches().contains(&entity.name))
            .map(|mutation| mutation.name)
            .collect();
        let kind = entity.authoring_kind();
        lines.push("    GeneratedEntityAuthoringSpec {".to_owned());
        lines.push(format!("        entity: {},", string_literal(entity.name)));
        lines.push(format!(
            "        kind: GeneratedAuthoringPolicyKind::{},",
            pascal_identifier(kind)
        ));
        lines.push(format!(
            "        rationale: {},",
            string_literal(entity.rationale())
        ));
        lines.push(format!(
            "        mutations: &[{}],",
            mutation_list(&mutations)
        ));
        lines.push(format!(
            "        content_mutation: {},",
            entity.content_mutation().map_or_else(
                || "None".to_owned(),
                |mutation| format!("Some(MutationName::{})", pascal_identifier(mutation))
            )
        ));
        lines.push(format!(
            "        planning_mutations: &[{}],",
            mutation_list(&entity.planning_mutations())
        ));
        lines.push(format!(
            "        command_mutations: &[{}],",
            mutation_list(&entity.command_mutations())
        ));
        lines.push(format!(
            "        lifecycle_mutations: &[{}],",
            mutation_list(&entity.lifecycle_mutations())
        ));
        let session_mnemonic_key = match (entity.owns_session(), session_key) {
            (true, Some(key)) => format!("Some({})", string_literal(key)),
            _ => "None".to_owned(),
        };
        lines.push(format!(
            "        session_mnemonic_key: {session_mnemonic_key},"
        ));
        let conflict_policy = match kind {
            "collaborative" => "Some(GeneratedAuthoringConflictPolicy::GeneratedFieldPolicy)",
            "optimisticDocument" => "Some(GeneratedAuthoringConflictPolicy::ExpectedRevision)",
            _ => "None",
        };
        lines.push(format!("        conflict_policy: {conflict_policy},"));
        lines.push("    },".to_owned());
    }
    lines.push("];".to_owned());
    lines.join("\n")
}

const REGISTRY_TYPES: &str = "#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedAuthoringPolicyKind {
    OptimisticDocument,
    Collaborative,
    CommandOwned,
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedAuthoringConflictPolicy {
    ExpectedRevision,
    GeneratedFieldPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedEntityAuthoringSpec {
    pub entity: &'static str,
    pub kind: GeneratedAuthoringPolicyKind,
    pub rationale: &'static str,
    pub mutations: &'static [MutationName],
    pub content_mutation: Option<MutationName>,
    pub planning_mutations: &'static [MutationName],
    pub command_mutations: &'static [MutationName],
    pub lifecycle_mutations: &'static [MutationName],
    pub session_mnemonic_key: Option<&'static str>,
    pub conflict_policy: Option<GeneratedAuthoringConflictPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedMaterializationPlan {
    KeyedCollection {
        collection_field: &'static str,
        item_field: &'static str,
        item_identity_field: &'static str,
        patch_identity_field: &'static str,
    },
    SequencedText {
        collection_field: &'static str,
        item_identity_field: &'static str,
        patch_identity_field: &'static str,
        snapshot_output_field: &'static str,
        sequence_field: &'static str,
        text_field: &'static str,
        patch_output_field: &'static str,
        delta_text_field: &'static str,
    },
    ReplaceOrRemove {
        snapshot_mode: GeneratedSnapshotMode,
        snapshot_omit_fields: &'static [&'static str],
        remove_mode: GeneratedRemoveMode,
        update_fields: Option<(&'static str, &'static str)>,
    },
    Replace {
        snapshot_mode: GeneratedSnapshotMode,
        snapshot_omit_fields: &'static [&'static str],
    },
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedSnapshotMode {
    Patch,
    Field(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedRemoveMode {
    NullSnapshot,
    NullField(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedProjectionSpec {
    pub name: ProjectionName,
    pub depends_on: &'static [&'static str],
    pub materialization: GeneratedMaterializationPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedMutationSpec {
    pub name: MutationName,
    pub touches: &'static [&'static str],
}
";

fn materialization_plan_lines(materialization: Materialization) -> Vec<String> {
    let field = |key: &str| string_literal(materialization.required(key));
    let snapshot_mode = || match materialization.setting("snapshotMode") {
        Some("patch") => "GeneratedSnapshotMode::Patch".to_owned(),
        _ => format!("GeneratedSnapshotMode::Field({})", field("snapshotField")),
    };
    let omit_fields = || string_list(&materialization.snapshot_omit_fields().unwrap_or_default());
    match materialization.strategy() {
        "keyedCollection" => vec![
            "        materialization: GeneratedMaterializationPlan::KeyedCollection {".to_owned(),
            format!(
                "            collection_field: {},",
                field("collectionField")
            ),
            format!("            item_field: {},", field("itemField")),
            format!(
                "            item_identity_field: {},",
                field("itemIdentityField")
            ),
            format!(
                "            patch_identity_field: {},",
                field("patchIdentityField")
            ),
            "        },".to_owned(),
        ],
        "sequencedText" => vec![
            "        materialization: GeneratedMaterializationPlan::SequencedText {".to_owned(),
            format!(
                "            collection_field: {},",
                field("collectionField")
            ),
            format!(
                "            item_identity_field: {},",
                field("itemIdentityField")
            ),
            format!(
                "            patch_identity_field: {},",
                field("patchIdentityField")
            ),
            format!(
                "            snapshot_output_field: {},",
                field("snapshotOutputField")
            ),
            format!("            sequence_field: {},", field("sequenceField")),
            format!("            text_field: {},", field("textField")),
            format!(
                "            patch_output_field: {},",
                field("patchOutputField")
            ),
            format!("            delta_text_field: {},", field("deltaTextField")),
            "        },".to_owned(),
        ],
        "replaceOrRemove" => {
            let remove_mode = if materialization.setting("removeMode") == Some("nullSnapshot") {
                "GeneratedRemoveMode::NullSnapshot".to_owned()
            } else {
                format!("GeneratedRemoveMode::NullField({})", field("removeField"))
            };
            let update_fields = match materialization
                .setting("updateField")
                .filter(|field| !field.is_empty())
            {
                Some(update_field) => format!(
                    "Some(({}, {}))",
                    string_literal(update_field),
                    field("updatesField")
                ),
                None => "None".to_owned(),
            };
            vec![
                "        materialization: GeneratedMaterializationPlan::ReplaceOrRemove {"
                    .to_owned(),
                format!("            snapshot_mode: {},", snapshot_mode()),
                format!("            snapshot_omit_fields: &[{}],", omit_fields()),
                format!("            remove_mode: {remove_mode},"),
                format!("            update_fields: {update_fields},"),
                "        },".to_owned(),
            ]
        }
        "replace" => vec![
            "        materialization: GeneratedMaterializationPlan::Replace {".to_owned(),
            format!("            snapshot_mode: {},", snapshot_mode()),
            format!("            snapshot_omit_fields: &[{}],", omit_fields()),
            "        },".to_owned(),
        ],
        "reset" => vec!["        materialization: GeneratedMaterializationPlan::Reset,".to_owned()],
        other => unreachable!("the meta-schema admits no strategy {other:?}"),
    }
}

const COLLABORATION_TYPES: &str = "#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedCollaborationStorageKind {
    Scalar,
    Text,
    OrderedList,
    StructuredList,
    StructuredMap,
    StructuredDocument,
    DerivedIdentity,
    DerivedRevision,
    KeyedSequence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedCollaborationValueCodec {
    Integer,
    Number,
    OptionalNumber,
    Boolean,
    String,
    OptionalString,
    PropertyText,
    StringList,
    StructuredJson,
    Identity,
    KeyedSequence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedCollaborationConflict {
    Immutable,
    Explicit,
    Merge,
    LastWriterWins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedCollaborationFieldSpec {
    pub path: &'static str,
    pub storage_kind: GeneratedCollaborationStorageKind,
    pub container: Option<&'static str>,
    pub container_template: Option<&'static str>,
    pub key: Option<&'static str>,
    pub identity_path: Option<&'static str>,
    pub identity_variable: Option<&'static str>,
    pub order_container: Option<&'static str>,
    pub item_container_template: Option<&'static str>,
    pub metadata_container: Option<&'static str>,
    pub metadata_container_template: Option<&'static str>,
    pub metadata_key: Option<&'static str>,
    pub codec: GeneratedCollaborationValueCodec,
    pub value_schema: Option<&'static str>,
    pub required: bool,
    pub conflict: GeneratedCollaborationConflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedCollaborationEntitySpec {
    pub name: &'static str,
    pub id_field: &'static str,
    pub substrate: &'static str,
    pub schema_version: u32,
    pub migration_ids: &'static [&'static str],
    pub authoring_projection: ProjectionName,
    pub import_mutation: MutationName,
    pub root_container: &'static str,
    pub fields: &'static [GeneratedCollaborationFieldSpec],
}
";

fn option_string(value: Option<&str>) -> String {
    value.map_or_else(
        || "None".to_owned(),
        |value| format!("Some({})", string_literal(value)),
    )
}

fn collaboration_metadata(definition: &Definition) -> String {
    let collaboration = definition.collaboration();
    let compatibility = collaboration.compatibility();
    let mut lines: Vec<String> = COLLABORATION_TYPES.lines().map(str::to_owned).collect();
    lines.push(String::new());
    lines.push(format!(
        "pub const COLLABORATION_DEFINITION_VERSION: u32 = {};",
        number_to_string(collaboration.version())
    ));
    lines.push(format!(
        "pub const COLLABORATION_MINIMUM_READER_VERSION: u32 = {};",
        number_to_string(compatibility.number_field("minimumReaderVersion"))
    ));
    lines.push(format!(
        "pub const COLLABORATION_MINIMUM_WRITER_VERSION: u32 = {};",
        number_to_string(compatibility.number_field("minimumWriterVersion"))
    ));
    lines.push(String::new());
    for entity in collaboration.entities() {
        let field_constant = format!("{}_COLLABORATION_FIELDS", constant_name(entity.name));
        lines.push(format!(
            "pub static {field_constant}: &[GeneratedCollaborationFieldSpec] = &["
        ));
        for field in entity.fields() {
            let storage = |key: &str| option_string(field.storage_setting(key));
            lines.push("    GeneratedCollaborationFieldSpec {".to_owned());
            lines.push(format!("        path: {},", string_literal(field.path)));
            lines.push(format!(
                "        storage_kind: GeneratedCollaborationStorageKind::{},",
                pascal_identifier(field.storage_kind())
            ));
            lines.push(format!("        container: {},", storage("container")));
            lines.push(format!(
                "        container_template: {},",
                storage("containerTemplate")
            ));
            lines.push(format!("        key: {},", storage("key")));
            lines.push(format!(
                "        identity_path: {},",
                storage("identityPath")
            ));
            lines.push(format!(
                "        identity_variable: {},",
                storage("identityVariable")
            ));
            lines.push(format!(
                "        order_container: {},",
                storage("orderContainer")
            ));
            lines.push(format!(
                "        item_container_template: {},",
                storage("itemContainerTemplate")
            ));
            lines.push(format!(
                "        metadata_container: {},",
                storage("metadataContainer")
            ));
            lines.push(format!(
                "        metadata_container_template: {},",
                storage("metadataContainerTemplate")
            ));
            lines.push(format!("        metadata_key: {},", storage("metadataKey")));
            lines.push(format!(
                "        codec: GeneratedCollaborationValueCodec::{},",
                pascal_identifier(field.codec())
            ));
            lines.push(format!(
                "        value_schema: {},",
                option_string(field.value_schema().map(ref_string))
            ));
            lines.push(format!("        required: {},", field.required()));
            lines.push(format!(
                "        conflict: GeneratedCollaborationConflict::{},",
                pascal_identifier(field.conflict())
            ));
            lines.push("    },".to_owned());
        }
        lines.push("];".to_owned());
        lines.push(String::new());
        let entity_id = definition
            .entity(entity.name)
            .expect("validation requires the collaboration entity to exist")
            .id();
        lines.push(format!(
            "pub static {}_COLLABORATION_SPEC: GeneratedCollaborationEntitySpec =",
            constant_name(entity.name)
        ));
        lines.push("    GeneratedCollaborationEntitySpec {".to_owned());
        lines.push(format!("        name: {},", string_literal(entity.name)));
        lines.push(format!("        id_field: {},", string_literal(entity_id)));
        lines.push(format!(
            "        substrate: {},",
            string_literal(entity.substrate())
        ));
        lines.push(format!(
            "        schema_version: {},",
            number_to_string(entity.schema_version())
        ));
        let migration_ids = entity.migration_ids();
        let migration_ids_line =
            format!("        migration_ids: &[{}],", string_list(&migration_ids));
        if utf16_len(&migration_ids_line) > 80 {
            lines.push("        migration_ids: &[".to_owned());
            for migration_id in &migration_ids {
                lines.push(format!("            {},", string_literal(migration_id)));
            }
            lines.push("        ],".to_owned());
        } else {
            lines.push(migration_ids_line);
        }
        lines.push(format!(
            "        authoring_projection: ProjectionName::{},",
            pascal_identifier(entity.authoring_projection())
        ));
        lines.push(format!(
            "        import_mutation: MutationName::{},",
            pascal_identifier(entity.import_mutation())
        ));
        lines.push(format!(
            "        root_container: {},",
            string_literal(entity.root_container())
        ));
        lines.push(format!("        fields: {field_constant},"));
        lines.push("    };".to_owned());
        lines.push(String::new());
    }
    lines.push(
        "pub static GENERATED_COLLABORATION_SPECS: &[GeneratedCollaborationEntitySpec] = &["
            .to_owned(),
    );
    for entity_name in collaboration.entity_names() {
        lines.push(format!(
            "    {}_COLLABORATION_SPEC,",
            constant_name(entity_name)
        ));
    }
    lines.extend(
        [
            "];",
            "",
            "pub fn generated_collaboration_spec(",
            "    entity: &str,",
            ") -> Option<&'static GeneratedCollaborationEntitySpec> {",
            "    GENERATED_COLLABORATION_SPECS",
            "        .iter()",
            "        .find(|spec| spec.name == entity)",
            "}",
        ]
        .map(str::to_owned),
    );
    lines.join("\n")
}

/// A Rust enum whose variants are PascalCase and whose wire values are exact.
fn render_enum(name: &str, values: &[String]) -> String {
    let variants: Vec<String> = values
        .iter()
        .map(|value| {
            format!(
                "    #[serde(rename = {})]\n    {},",
                string_literal(value),
                pascal_identifier(value)
            )
        })
        .collect();
    format!(
        "#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]\npub enum {name} {{\n{}\n}}",
        variants.join("\n")
    )
}

/// One `$defs` entry as a serde type or a transparent alias.
fn render_definition(definition_name: &str, schema: &Object) -> Result<String> {
    let type_name = pascal_identifier(definition_name);
    if has_type(schema, "json") {
        return Ok(format!("pub type {type_name} = JsonValue;"));
    }
    if matches!(schema.get("$ref"), Some(Json::String(_))) {
        return Ok(format!(
            "pub type {type_name} = {};",
            schema_ref_type_name(schema)?
        ));
    }
    if let Some(members) = primitive_union_types(schema)? {
        return primitive_union(definition_name, &members);
    }
    if !has_type(schema, "object") {
        return refuse(format!(
            "Rust generation currently expects $defs.{definition_name} to be an object schema."
        ));
    }
    if object_at(schema, "oneOf").is_some() {
        return tagged_union(definition_name, schema);
    }
    // A record-shaped `$def` names one open map, so a value schema used at many
    // sites is written once.
    if let Some(additional) = object_at(schema, "additionalProperties") {
        let value = rust_type(definition_name, "value", additional, false)?;
        return Ok(format!("pub type {type_name} = HashMap<String, {value}>;"));
    }
    let Some(properties) = object_at(schema, "properties") else {
        return refuse(format!(
            "Rust generation expects $defs.{definition_name} to declare properties."
        ));
    };
    let required = read_string_array(
        schema.get("required"),
        &format!("{definition_name}.required"),
    )?;
    let mut fields = Vec::new();
    for (property_name, property_schema) in properties.iter() {
        let property_schema = property_schema
            .as_object()
            .expect("validation requires object property schemas");
        fields.push(render_field(
            definition_name,
            property_name,
            property_schema,
            required.contains(&property_name),
            "    ",
            "pub ",
        )?);
    }
    let derive = "#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]\n#[serde(deny_unknown_fields)]";
    if fields.is_empty() {
        // Unit-like params still reject unknown fields on deserialisation.
        return Ok(format!("{derive}\npub struct {type_name} {{}}"));
    }
    Ok(format!(
        "{derive}\npub struct {type_name} {{\n{}\n}}",
        fields.join("\n")
    ))
}

fn primitive_union(definition_name: &str, members: &[&str]) -> Result<String> {
    let mut variants = Vec::new();
    for member in members {
        let rust_type = match *member {
            "string" => "String",
            "boolean" => "bool",
            "integer" => "i64",
            "number" => "f64",
            other => {
                return refuse(format!(
                    "Cannot render unsupported Rust primitive union member {}.",
                    string_literal(other)
                ))
            }
        };
        variants.push(format!("    {}({rust_type}),", pascal_identifier(member)));
    }
    Ok([
        "#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]"
            .to_owned(),
        "#[serde(untagged)]".to_owned(),
        format!("pub enum {} {{", pascal_identifier(definition_name)),
        variants.join("\n"),
        "}".to_owned(),
    ]
    .join("\n"))
}

/// A `$defs` tagged union as a serde internally tagged enum. Variant fields
/// carry no visibility modifier.
fn tagged_union(definition_name: &str, schema: &Object) -> Result<String> {
    let discriminator = schema
        .get("discriminator")
        .and_then(Json::as_str)
        .expect("validation requires a discriminator");
    let variants = object_at(schema, "oneOf").expect("checked by the caller");
    let empty = Object::new();
    let mut rendered = Vec::new();
    for (tag, variant) in variants.iter() {
        let variant = variant
            .as_object()
            .expect("validation requires object variants");
        let properties = object_at(variant, "properties").unwrap_or(&empty);
        let required = read_string_array(
            variant.get("required"),
            &format!("{definition_name}.{tag}.required"),
        )?;
        let mut fields = Vec::new();
        let mut inline_fields = Vec::new();
        for (property_name, property_schema) in properties.iter() {
            let property_schema = property_schema
                .as_object()
                .expect("validation requires object property schemas");
            let is_required = required.contains(&property_name);
            fields.push(render_field(
                definition_name,
                property_name,
                property_schema,
                is_required,
                "        ",
                "",
            )?);
            let base = rust_type(definition_name, property_name, property_schema, false)?;
            inline_fields.push(format!(
                "{}: {}",
                rust_field_identifier(property_name),
                if is_required {
                    base
                } else {
                    format!("Option<{base}>")
                }
            ));
        }
        let fields = fields.join("\n");
        let inline_body = inline_fields.join(", ");
        // rustfmt inlines a struct variant whose body fits
        // `struct_variant_width` (35) and carries no attributes.
        let inline = !fields.contains("#[") && utf16_len(&inline_body) <= 35;
        let body = if fields.is_empty() {
            String::new()
        } else if inline {
            format!(" {inline_body} ")
        } else {
            format!("\n{fields}\n    ")
        };
        rendered.push(format!(
            "    #[serde(rename = {})]\n    {} {{{body}}},",
            string_literal(tag),
            pascal_identifier(tag)
        ));
    }
    Ok([
        "#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]"
            .to_owned(),
        format!(
            "#[serde(tag = {}, deny_unknown_fields)]",
            string_literal(discriminator)
        ),
        format!("pub enum {} {{", pascal_identifier(definition_name)),
        rendered.join("\n"),
        "}".to_owned(),
    ]
    .join("\n"))
}

/// One field, with serde attributes for a renamed or optional wire property.
fn render_field(
    definition_name: &str,
    property_name: &str,
    property_schema: &Object,
    required: bool,
    indent: &str,
    visibility: &str,
) -> Result<String> {
    let rust_name = rust_field_identifier(property_name);
    let mut attributes = Vec::new();
    if rust_name.strip_prefix("r#").unwrap_or(&rust_name) != property_name {
        attributes.push(format!(
            "{indent}#[serde(rename = {})]",
            string_literal(property_name)
        ));
    }
    if !required {
        // `Option<T>` omitted when absent, matching the TypeScript `?` field.
        attributes.push(format!(
            "{indent}#[serde(skip_serializing_if = \"Option::is_none\")]"
        ));
    }
    let validation = schemars_arguments(property_schema);
    if !validation.is_empty() {
        attributes.push(format!("{indent}#[schemars({})]", validation.join(", ")));
    }
    let base = rust_type(definition_name, property_name, property_schema, false)?;
    let field_type = if required {
        base
    } else {
        format!("Option<{base}>")
    };
    let prefix: String = attributes
        .iter()
        .map(|attribute| format!("{attribute}\n"))
        .collect();
    Ok(format!(
        "{prefix}{indent}{visibility}{rust_name}: {field_type},"
    ))
}

/// Validation keywords reach the generated schema through `schemars`
/// attributes; `inner` applies to array items.
fn schemars_arguments(schema: &Object) -> Vec<String> {
    let mut arguments = Vec::new();
    if let Some(constant) = schema.get("const") {
        arguments.push(format!("extend(\"const\" = {})", constant.stringify()));
    }
    if let Some(default) = schema.get("default") {
        arguments.push(format!("extend(\"default\" = {})", default.stringify()));
    }
    if let Some(min_length) = schema.get("minLength").and_then(Json::as_f64) {
        arguments.push(format!("length(min = {})", number_to_string(min_length)));
    }
    if let Some(items) = object_at(schema, "items") {
        let inner = schemars_arguments(items);
        if !inner.is_empty() {
            arguments.push(format!("inner({})", inner.join(", ")));
        }
    }
    arguments
}

/// The Rust type of a field. A field holding its own type directly is boxed;
/// `Vec` and `HashMap` already are indirections.
fn rust_type(
    definition_name: &str,
    property_name: &str,
    schema: &Object,
    behind_indirection: bool,
) -> Result<String> {
    if matches!(schema.get("$ref"), Some(Json::String(_))) {
        let referenced = schema_ref_type_name(schema)?;
        return Ok(
            if referenced == pascal_identifier(definition_name) && !behind_indirection {
                format!("Box<{referenced}>")
            } else {
                referenced
            },
        );
    }
    if schema.get("enum").and_then(Json::as_array).is_some() {
        return Ok(rust_property_enum_name(definition_name, property_name));
    }
    if primitive_union_types(schema)?.is_some() {
        return refuse(format!(
            "Rust generation does not support anonymous primitive unions at {definition_name}.{property_name}. Use a $defs reference."
        ));
    }
    let schema_type = schema.get("type");
    Ok(match schema_type.and_then(Json::as_str) {
        Some("array") => {
            let items = object_at(schema, "items").expect("validation requires array items");
            format!("Vec<{}>", rust_type(definition_name, property_name, items, true)?)
        }
        Some("string") => "String".to_owned(),
        Some("boolean") => "bool".to_owned(),
        Some("integer") => "i64".to_owned(),
        Some("number") => "f64".to_owned(),
        Some("json") => "JsonValue".to_owned(),
        Some("object") => match object_at(schema, "additionalProperties") {
            Some(additional) => format!(
                "HashMap<String, {}>",
                rust_type(definition_name, property_name, additional, true)?
            ),
            None => {
                return refuse(format!(
                    "Rust generation does not support anonymous nested object fields at {definition_name}.{property_name}. Use a $defs reference."
                ))
            }
        },
        _ => {
            return refuse(format!(
                "Cannot render unsupported Rust schema type {}.",
                stringify_or_undefined(schema_type)
            ))
        }
    })
}

fn string_slice_constant(name: &str, values: &[&str]) -> String {
    if values.is_empty() {
        return format!("pub const {name}: &[&str] = &[];");
    }
    let inline = format!("pub const {name}: &[&str] = &[{}];", string_list(values));
    if utf16_len(&inline) <= 100 {
        return inline;
    }
    let rendered: Vec<String> = values
        .iter()
        .map(|value| format!("    {},", string_literal(value)))
        .collect();
    format!(
        "pub const {name}: &[&str] = &[\n{}\n];",
        rendered.join("\n")
    )
}

fn string_constant(name: &str, value: &str) -> String {
    let inline = format!("pub const {name}: &str = {};", string_literal(value));
    if utf16_len(&inline) <= 100 {
        return inline;
    }
    format!("pub const {name}: &str =\n    {};", string_literal(value))
}

/// One constant per public name, so dispatch code depends on the definition
/// rather than on repeated wire strings.
fn name_constants(suffix: &str, values: &[&str]) -> Vec<String> {
    values
        .iter()
        .map(|value| string_constant(&name_constant_identifier(value, suffix), value))
        .collect()
}

/// An exhaustively matchable enum over public names, with `as_str` and `parse`.
fn name_enum(enum_name: &str, suffix: &str, values: &[&str]) -> String {
    let variants: Vec<String> = values
        .iter()
        .map(|value| format!("    {},", pascal_identifier(value)))
        .collect();
    let as_str_arms: Vec<String> = values
        .iter()
        .map(|value| {
            format!(
                "            Self::{} => {{\n                {}\n            }},",
                pascal_identifier(value),
                name_constant_identifier(value, suffix)
            )
        })
        .collect();
    let parse_arms: Vec<String> = values
        .iter()
        .map(|value| {
            format!(
                "            {} => {{\n                Some(Self::{})\n            }},",
                name_constant_identifier(value, suffix),
                pascal_identifier(value)
            )
        })
        .collect();
    [
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]".to_owned(),
        format!("pub enum {enum_name} {{"),
        variants.join("\n"),
        "}".to_owned(),
        String::new(),
        "#[rustfmt::skip]".to_owned(),
        format!("impl {enum_name} {{"),
        "    pub const fn as_str(self) -> &'static str {".to_owned(),
        "        match self {".to_owned(),
        as_str_arms.join("\n"),
        "        }".to_owned(),
        "    }".to_owned(),
        String::new(),
        "    pub fn parse(value: &str) -> Option<Self> {".to_owned(),
        "        match value {".to_owned(),
        parse_arms.join("\n"),
        "            _ => None,".to_owned(),
        "        }".to_owned(),
        "    }".to_owned(),
        "}".to_owned(),
    ]
    .join("\n")
}
