//! The Rust model module: serde types for every `$defs` entry, dispatch enums,
//! and the registry and collaboration plan constants.

use crate::config::Project;
use crate::definition::{Definition, Fields, Materialization};
use crate::error::{refuse, Result};
use crate::json::{number_to_string, string_literal, Json, Object};
use crate::layout::{self, Expr};
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
            "pub use clerkenwell_schema::{",
            "    GeneratedAuthoringConflictPolicy, GeneratedAuthoringPolicyKind, GeneratedCollaborationConflict,",
            "    GeneratedCollaborationEntitySpec, GeneratedCollaborationFieldSpec,",
            "    GeneratedCollaborationStorageKind, GeneratedCollaborationValueCodec,",
            "    GeneratedEntityAuthoringSpec, GeneratedMaterializationPlan, GeneratedMutationSpec,",
            "    GeneratedProjectionSpec, GeneratedRemoveMode, GeneratedSnapshotMode,",
            "};",
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
        // Variants wrap arbitrary schema-defined payloads, so their sizes vary
        // by design.
        "#[allow(clippy::large_enum_variant)]".to_owned(),
        format!("pub enum {enum_name} {{"),
    ];
    for (name, type_name) in entries {
        lines.push(rename_attribute("    ", name));
        lines.push(layout::wrap_last_argument(
            &format!("    {}(", pascal_identifier(name)),
            type_name,
            "),",
        ));
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

fn literal(value: &str) -> Expr {
    Expr::atom(string_literal(value))
}

fn literals(values: &[&str]) -> Expr {
    Expr::slice(values.iter().map(|value| literal(value)).collect())
}

fn mutation_path(name: &str) -> Expr {
    Expr::atom(name_constant_identifier(name, "MUTATION"))
}

fn projection_path(name: &str) -> Expr {
    Expr::atom(name_constant_identifier(name, "PROJECTION"))
}

fn mutation_paths(names: &[&str]) -> Expr {
    Expr::slice(names.iter().map(|name| mutation_path(name)).collect())
}

fn spec(name: &str, fields: Vec<(&str, Expr)>) -> Expr {
    Expr::Struct(
        name.to_owned(),
        fields
            .into_iter()
            .map(|(field, value)| (field.to_owned(), value))
            .collect(),
    )
}

fn registry_metadata(definition: &Definition, project: &Project) -> String {
    let projections = definition
        .projections()
        .map(|projection| {
            spec(
                "GeneratedProjectionSpec",
                vec![
                    ("name", projection_path(projection.name)),
                    ("depends_on", literals(&projection.depends_on())),
                    (
                        "materialization",
                        materialization_plan(projection.materialization()),
                    ),
                ],
            )
        })
        .collect();
    let mutations = definition
        .mutations()
        .map(|mutation| {
            spec(
                "GeneratedMutationSpec",
                vec![
                    ("name", mutation_path(mutation.name)),
                    ("touches", literals(&mutation.touches())),
                ],
            )
        })
        .collect();
    let session_key = project
        .authoring_session_mnemonic
        .as_deref()
        .and_then(|name| definition.mnemonic(name))
        .map(|mnemonic| mnemonic.key());
    let entities = definition
        .entities()
        .map(|entity| {
            let touching: Vec<&str> = definition
                .mutations()
                .filter(|mutation| mutation.touches().contains(&entity.name))
                .map(|mutation| mutation.name)
                .collect();
            let kind = entity.authoring_kind();
            let conflict_policy = match kind {
                "collaborative" => Some("GeneratedFieldPolicy"),
                "optimisticDocument" => Some("ExpectedRevision"),
                _ => None,
            };
            spec(
                "GeneratedEntityAuthoringSpec",
                vec![
                    ("entity", literal(entity.name)),
                    (
                        "kind",
                        Expr::atom(format!(
                            "GeneratedAuthoringPolicyKind::{}",
                            pascal_identifier(kind)
                        )),
                    ),
                    ("rationale", literal(entity.rationale())),
                    ("mutations", mutation_paths(&touching)),
                    (
                        "content_mutation",
                        Expr::option(entity.content_mutation().map(mutation_path)),
                    ),
                    (
                        "planning_mutations",
                        mutation_paths(&entity.planning_mutations()),
                    ),
                    (
                        "command_mutations",
                        mutation_paths(&entity.command_mutations()),
                    ),
                    (
                        "lifecycle_mutations",
                        mutation_paths(&entity.lifecycle_mutations()),
                    ),
                    (
                        "session_mnemonic_key",
                        Expr::option(session_key.filter(|_| entity.owns_session()).map(literal)),
                    ),
                    (
                        "conflict_policy",
                        Expr::option(conflict_policy.map(|policy| {
                            Expr::atom(format!("GeneratedAuthoringConflictPolicy::{policy}"))
                        })),
                    ),
                ],
            )
        })
        .collect();
    let lines = [
        layout::item(
            "pub static GENERATED_PROJECTION_SPECS: &[GeneratedProjectionSpec] =",
            &Expr::slice(projections),
        ),
        String::new(),
        layout::item(
            "pub static GENERATED_MUTATION_SPECS: &[GeneratedMutationSpec] =",
            &Expr::slice(mutations),
        ),
        String::new(),
        layout::item(
            "pub static GENERATED_ENTITY_AUTHORING_SPECS: &[GeneratedEntityAuthoringSpec] =",
            &Expr::slice(entities),
        ),
    ];
    lines.join("\n")
}

fn materialization_plan(materialization: Materialization) -> Expr {
    let field = |key: &str| literal(materialization.required(key));
    let snapshot_mode = || match materialization.setting("snapshotMode") {
        Some("patch") => Expr::atom("GeneratedSnapshotMode::Patch"),
        _ => Expr::Call(
            "GeneratedSnapshotMode::Field".to_owned(),
            vec![field("snapshotField")],
        ),
    };
    let omit_fields = || literals(&materialization.snapshot_omit_fields().unwrap_or_default());
    let plan = |variant: &str, fields: Vec<(&str, Expr)>| {
        spec(&format!("GeneratedMaterializationPlan::{variant}"), fields)
    };
    match materialization.strategy() {
        "keyedCollection" => plan(
            "KeyedCollection",
            vec![
                ("collection_field", field("collectionField")),
                ("item_field", field("itemField")),
                ("item_identity_field", field("itemIdentityField")),
                ("patch_identity_field", field("patchIdentityField")),
            ],
        ),
        "sequencedText" => plan(
            "SequencedText",
            vec![
                ("collection_field", field("collectionField")),
                ("item_identity_field", field("itemIdentityField")),
                ("patch_identity_field", field("patchIdentityField")),
                ("snapshot_output_field", field("snapshotOutputField")),
                ("sequence_field", field("sequenceField")),
                ("text_field", field("textField")),
                ("patch_output_field", field("patchOutputField")),
                ("delta_text_field", field("deltaTextField")),
            ],
        ),
        "replaceOrRemove" => {
            let remove_mode = if materialization.setting("removeMode") == Some("nullSnapshot") {
                Expr::atom("GeneratedRemoveMode::NullSnapshot")
            } else {
                Expr::Call(
                    "GeneratedRemoveMode::NullField".to_owned(),
                    vec![field("removeField")],
                )
            };
            let update_fields = materialization
                .setting("updateField")
                .filter(|update_field| !update_field.is_empty())
                .map(|update_field| {
                    Expr::Tuple(vec![literal(update_field), field("updatesField")])
                });
            plan(
                "ReplaceOrRemove",
                vec![
                    ("snapshot_mode", snapshot_mode()),
                    ("snapshot_omit_fields", omit_fields()),
                    ("remove_mode", remove_mode),
                    ("update_fields", Expr::option(update_fields)),
                ],
            )
        }
        "replace" => plan(
            "Replace",
            vec![
                ("snapshot_mode", snapshot_mode()),
                ("snapshot_omit_fields", omit_fields()),
            ],
        ),
        "reset" => Expr::atom("GeneratedMaterializationPlan::Reset"),
        other => unreachable!("the meta-schema admits no strategy {other:?}"),
    }
}

fn collaboration_metadata(definition: &Definition) -> String {
    let collaboration = definition.collaboration();
    let compatibility = collaboration.compatibility();
    let mut lines = Vec::new();
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
        let fields = entity
            .fields()
            .map(|field| {
                let storage = |key: &str| Expr::option(field.storage_setting(key).map(literal));
                spec(
                    "GeneratedCollaborationFieldSpec",
                    vec![
                        ("path", literal(field.path)),
                        (
                            "storage_kind",
                            Expr::atom(format!(
                                "GeneratedCollaborationStorageKind::{}",
                                pascal_identifier(field.storage_kind())
                            )),
                        ),
                        ("container", storage("container")),
                        ("container_template", storage("containerTemplate")),
                        ("key", storage("key")),
                        ("identity_path", storage("identityPath")),
                        ("identity_variable", storage("identityVariable")),
                        ("order_container", storage("orderContainer")),
                        ("item_container_template", storage("itemContainerTemplate")),
                        ("metadata_container", storage("metadataContainer")),
                        (
                            "metadata_container_template",
                            storage("metadataContainerTemplate"),
                        ),
                        ("metadata_key", storage("metadataKey")),
                        (
                            "codec",
                            Expr::atom(format!(
                                "GeneratedCollaborationValueCodec::{}",
                                pascal_identifier(field.codec())
                            )),
                        ),
                        (
                            "value_schema",
                            Expr::option(
                                field
                                    .value_schema()
                                    .map(|schema| literal(ref_string(schema))),
                            ),
                        ),
                        ("required", Expr::atom(field.required().to_string())),
                        (
                            "conflict",
                            Expr::atom(format!(
                                "GeneratedCollaborationConflict::{}",
                                pascal_identifier(field.conflict())
                            )),
                        ),
                    ],
                )
            })
            .collect();
        lines.push(layout::item(
            &format!("pub static {field_constant}: &[GeneratedCollaborationFieldSpec] ="),
            &Expr::slice(fields),
        ));
        lines.push(String::new());
        let entity_id = definition
            .entity(entity.name)
            .expect("validation requires the collaboration entity to exist")
            .id();
        let entity_spec = spec(
            "GeneratedCollaborationEntitySpec",
            vec![
                ("name", literal(entity.name)),
                ("id_field", literal(entity_id)),
                ("substrate", literal(entity.substrate())),
                (
                    "schema_version",
                    Expr::atom(number_to_string(entity.schema_version())),
                ),
                ("migration_ids", literals(&entity.migration_ids())),
                (
                    "authoring_projection",
                    projection_path(entity.authoring_projection()),
                ),
                ("import_mutation", mutation_path(entity.import_mutation())),
                ("root_container", literal(entity.root_container())),
                ("fields", Expr::atom(field_constant)),
            ],
        );
        lines.push(layout::item(
            &format!(
                "pub static {}_COLLABORATION_SPEC: GeneratedCollaborationEntitySpec =",
                constant_name(entity.name)
            ),
            &entity_spec,
        ));
        lines.push(String::new());
    }
    let specs = collaboration
        .entity_names()
        .into_iter()
        .map(|name| Expr::atom(format!("{}_COLLABORATION_SPEC", constant_name(name))))
        .collect();
    lines.push(layout::item(
        "pub static GENERATED_COLLABORATION_SPECS: &[GeneratedCollaborationEntitySpec] =",
        &Expr::slice(specs),
    ));
    lines.extend(
        [
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
                "{}\n    {},",
                rename_attribute("    ", value),
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
            "{}\n    {} {{{body}}},",
            rename_attribute("    ", tag),
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
        attributes.push(rename_attribute(indent, property_name));
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
        "{prefix}{}",
        layout::declaration(indent, &format!("{visibility}{rust_name}:"), &field_type)
    ))
}

/// `#[serde(rename = "value")]`, its argument on a line of its own when the
/// attribute is too wide, as rustfmt breaks it.
fn rename_attribute(indent: &str, value: &str) -> String {
    let argument = format!("rename = {}", string_literal(value));
    let line = format!("{indent}#[serde({argument})]");
    if layout::fits_attribute(&line) {
        line
    } else {
        format!("{indent}#[serde(\n{indent}    {argument}\n{indent})]")
    }
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
    layout::item(&format!("pub const {name}: &[&str] ="), &literals(values))
}

/// `pub const NAME: &str = "value";`, broken after `=` when too wide, and
/// after `NAME:` when even the head is.
fn string_constant(name: &str, value: &str) -> String {
    let head = format!("pub const {name}: &str =");
    let literal = string_literal(value);
    let inline = format!("{head} {literal};");
    if utf16_len(&inline) <= 100 {
        return inline;
    }
    if layout::fits(&head) {
        return format!("{head}\n    {literal};");
    }
    format!("pub const {name}:\n    &str = {literal};")
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
