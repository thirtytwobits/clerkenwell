//! The Rust model module: serde types for every `$defs` entry, dispatch enums,
//! and the registry and collaboration plan constants.

use std::collections::HashSet;

use crate::config::Project;
use crate::definition::{Definition, Materialization};
use crate::error::{refuse, Result};
use crate::expand::SHARED;
use crate::json::{number_to_string, Json, Object};
use crate::json_schema::schema_document;
use crate::layout::{self, Expr};
use crate::names::{
    constant_name, name_constant_identifier, pascal_identifier, rust_field_identifier,
    rust_property_enum_name, rust_record_constraints_name, schema_constant_identifier, utf16_len,
};
use crate::schema::{
    has_type, is_externally_tagged, is_null_schema, nullable_inner, object_at,
    other_members_schema, primitive_union_types, read_string_array, record_values_schema,
    ref_string, schema_ref_name, schema_ref_type_name, stringify_or_undefined,
};

/// A helper enum generated for a string-enum property.
pub(crate) struct RustPropertyEnum {
    pub name: String,
    /// The `$defs` entry declaring the property.
    pub def: String,
    pub source: String,
    pub values: Vec<String>,
    /// The string enum's schema, whose validation keywords and default the
    /// enum carries.
    pub schema: Object,
}

/// Every object property, every property of a tagged-union variant, and every
/// externally tagged variant's value, with a string enum (directly, nullable,
/// or as array items). Each is named after its owner and property so two
/// `kind` fields stay distinct; a variant's owner is its union and its tag, so
/// two variants' `kind` fields do too. An externally tagged variant's value is
/// named after its union and its tag.
pub(crate) fn collect_rust_property_enums(definition: &Definition) -> Vec<RustPropertyEnum> {
    let mut enums = Vec::new();
    for (definition_name, schema) in definition.def_entries() {
        if is_externally_tagged(schema) {
            for (tag, variant) in object_at(schema, "oneOf")
                .into_iter()
                .flat_map(Object::iter)
            {
                let Some(variant) = variant.as_object() else {
                    continue;
                };
                if let Some(enum_schema) = property_enum_schema(variant) {
                    enums.push(RustPropertyEnum {
                        name: rust_property_enum_name(definition_name, tag),
                        def: definition_name.to_owned(),
                        source: format!("$defs.{definition_name}.oneOf.{tag}"),
                        values: string_enum_values(enum_schema).expect("an enum schema"),
                        schema: enum_schema.clone(),
                    });
                }
            }
        }
        let union_variants = object_at(schema, "oneOf")
            .into_iter()
            .flat_map(Object::iter);
        let mut owners = Vec::new();
        if has_type(schema, "object") {
            if let Some(properties) = object_at(schema, "properties") {
                owners.push((
                    definition_name.to_owned(),
                    format!("$defs.{definition_name}.properties"),
                    Some(properties),
                    other_members_schema(schema),
                ));
            }
        }
        for (tag, variant) in union_variants {
            let Some(variant) = variant.as_object() else {
                continue;
            };
            if !has_type(variant, "object") {
                continue;
            }
            let properties = object_at(variant, "properties");
            let other_members = object_at(variant, "additionalProperties");
            if properties.is_some() || other_members.is_some() {
                owners.push((
                    variant_enum_owner(definition_name, tag),
                    format!("$defs.{definition_name}.oneOf.{tag}.properties"),
                    properties,
                    other_members,
                ));
            }
        }
        for (owner, path, properties, other_members) in owners {
            if owner != definition_name {
                if let Some(enum_schema) = other_members.and_then(property_enum_schema) {
                    enums.push(RustPropertyEnum {
                        name: rust_property_enum_name(&owner, OTHER_MEMBERS_FIELD),
                        def: definition_name.to_owned(),
                        source: format!("{path}.additionalProperties"),
                        values: string_enum_values(enum_schema).expect("an enum schema"),
                        schema: enum_schema.clone(),
                    });
                }
            }
            for (property_name, property_schema) in properties.into_iter().flat_map(Object::iter) {
                let Some(schema) = property_schema.as_object().and_then(property_enum_schema)
                else {
                    continue;
                };
                enums.push(RustPropertyEnum {
                    name: rust_property_enum_name(&owner, property_name),
                    def: definition_name.to_owned(),
                    source: format!("{path}.{property_name}"),
                    values: string_enum_values(schema).expect("an enum schema"),
                    schema: schema.clone(),
                });
            }
        }
        if !has_type(schema, "object") {
            continue;
        }
        // The values of a record `$def` are named `value`, an open object's
        // other members after their field.
        let values = record_values_schema(schema)
            .map(|values| ("value", values))
            .or_else(|| other_members_schema(schema).map(|values| (OTHER_MEMBERS_FIELD, values)));
        if let Some((name, values)) = values {
            if let Some(schema) = property_enum_schema(values) {
                enums.push(RustPropertyEnum {
                    name: rust_property_enum_name(definition_name, name),
                    def: definition_name.to_owned(),
                    source: format!("$defs.{definition_name}.additionalProperties"),
                    values: string_enum_values(schema).expect("an enum schema"),
                    schema: schema.clone(),
                });
            }
        }
    }
    enums
}

/// A property's string enum schema, declared directly or within its array
/// items and record values at any depth, each of which the Rust type names
/// after the property.
fn property_enum_schema(property_schema: &Object) -> Option<&Object> {
    if string_enum_values(property_schema).is_some() && has_type(property_schema, "string") {
        return Some(property_schema);
    }
    if let Some(member) = nullable_member(property_schema) {
        return property_enum_schema(member);
    }
    has_type(property_schema, "array")
        .then(|| object_at(property_schema, "items"))
        .flatten()
        .and_then(property_enum_schema)
        .or_else(|| record_values_schema(property_schema).and_then(property_enum_schema))
}

/// The values of `schema`'s string enum.
fn string_enum_values(schema: &Object) -> Option<Vec<String>> {
    let values = schema.get("enum").and_then(Json::as_array)?;
    Some(
        values
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_owned)
            .collect(),
    )
}

/// The struct each open union variant is generated as, named after its union
/// and tag, with the definition path it comes from.
pub(crate) fn variant_structs(definition: &Definition) -> Vec<(String, String)> {
    let mut structs = Vec::new();
    for (definition_name, schema) in definition.def_entries() {
        let external = is_externally_tagged(schema);
        for (tag, variant) in object_at(schema, "oneOf")
            .into_iter()
            .flat_map(Object::iter)
        {
            let Some(variant) = variant.as_object() else {
                continue;
            };
            let object = has_type(variant, "object") && object_at(variant, "properties").is_some();
            let open = object_at(variant, "additionalProperties").is_some();
            if open && (object || !external) {
                structs.push((
                    variant_enum_owner(definition_name, tag),
                    format!("$defs.{definition_name}.oneOf.{tag}"),
                ));
            }
        }
    }
    structs
}

/// The non-null member of an `anyOf` nullable schema, borrowed.
fn nullable_member(schema: &Object) -> Option<&Object> {
    let members = schema.get("anyOf").and_then(Json::as_array)?;
    nullable_inner(schema)?;
    members
        .iter()
        .find(|member| !is_null_schema(member))
        .and_then(Json::as_object)
}

/// What a tagged-union variant's property enums, and its struct when it has
/// one of its own, are named after: the union and the variant's tag.
pub(crate) fn variant_enum_owner(definition_name: &str, tag: &str) -> String {
    format!(
        "{}{}",
        pascal_identifier(definition_name),
        pascal_identifier(tag)
    )
}

/// The `clerkenwell_schema` items the model re-exports.
const SCHEMA_REEXPORTS: [&str; 28] = [
    "ActorKind",
    "AuthoringPeerBlock",
    "AuthoringState",
    "AuthoringStatePatch",
    "AuthoringStatePatchKind",
    "CollaborationExchangeMode",
    "DeclaredMemberError",
    "DeclaredMembers",
    "GeneratedAuthoringPolicyKind",
    "GeneratedCollaborationConflict",
    "GeneratedCollaborationEntitySpec",
    "GeneratedCollaborationFieldSpec",
    "GeneratedCollaborationStorageKind",
    "GeneratedCollaborationValueCodec",
    "GeneratedCommandSpec",
    "GeneratedEntityAuthoringSpec",
    "GeneratedMaterializationPlan",
    "GeneratedMutationSpec",
    "GeneratedProjectionSpec",
    "GeneratedRemoveMode",
    "GeneratedSnapshotMode",
    "GeneratedTransportValue",
    "GeneratedWriterConflict",
    "OtherMembers",
    "RecordConstraints",
    "RecordMembers",
    "SchemaConstraint",
    "SchemaConstraintValue",
];

/// The Rust field of a generated open object that holds its other members.
pub(crate) const OTHER_MEMBERS_FIELD: &str = "other_members";

/// The names the model's other imports bind, the last two only where its
/// body uses them.
const OTHER_IMPORTS: [&str; 4] = ["Deserialize", "Serialize", "JsonValue", "BTreeMap"];

/// Every name the model's imports bind, which no `$defs` entry may generate.
pub(crate) fn import_bindings() -> impl Iterator<Item = &'static str> {
    SCHEMA_REEXPORTS.into_iter().chain(OTHER_IMPORTS)
}

/// `items`, each followed by a comma, filled into lines of at most `width`
/// columns after `indent`, as rustfmt lays out a use list.
fn filled(items: &[&str], indent: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for item in items {
        let piece = format!("{item},");
        if line.is_empty() {
            line = format!("{indent}{piece}");
        } else if line.len() + 1 + piece.len() <= width {
            line.push(' ');
            line.push_str(&piece);
        } else {
            lines.push(std::mem::replace(&mut line, format!("{indent}{piece}")));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The model crate's source.
pub fn render_model_module(definition: &Definition, project: &Project) -> Result<String> {
    let mut lines: Vec<String> = Vec::new();

    // TypeScript inlines literal unions; Rust names an enum for each, before
    // the structs that use them. The authoring states' types, enums included,
    // are `clerkenwell_schema`'s.
    for property_enum in collect_rust_property_enums(definition)
        .into_iter()
        .filter(|property_enum| !SHARED.contains(&property_enum.def.as_str()))
    {
        lines.push(with_constraints(
            definition,
            render_enum(
                &property_enum.name,
                &property_enum.values,
                object_at(&property_enum.schema, "enumDescriptions"),
            ),
            &property_enum.schema,
        ));
        lines.push(String::new());
    }

    for (definition_name, schema) in definition.declared_def_entries() {
        lines.push(render_definition(definition, definition_name, schema)?);
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
    let command_names: Vec<&str> = definition.commands().map(|command| command.name).collect();
    lines.push(string_constant("NAMESPACE", definition.namespace()));
    lines.push(String::new());
    lines.extend(name_constants("ENTITY", &entity_names));
    lines.push(String::new());
    lines.extend(name_constants("PROJECTION", &projection_names));
    lines.push(String::new());
    lines.extend(name_constants("MUTATION", &mutation_names));
    lines.push(String::new());
    if !command_names.is_empty() {
        lines.extend(name_constants("COMMAND", &command_names));
        lines.push(String::new());
    }
    lines.push(name_enum("ProjectionName", "PROJECTION", &projection_names));
    lines.push(String::new());
    lines.push(name_enum("MutationName", "MUTATION", &mutation_names));
    lines.push(String::new());
    lines.push(name_enum("CommandName", "COMMAND", &command_names));
    lines.push(String::new());
    lines.push(string_slice_constant("ENTITY_NAMES", &entity_names));
    lines.push(string_slice_constant("PROJECTION_NAMES", &projection_names));
    lines.push(string_slice_constant("MUTATION_NAMES", &mutation_names));
    lines.push(string_slice_constant("COMMAND_NAMES", &command_names));
    lines.push(String::new());
    let projections: Vec<_> = definition.projections().collect();
    lines.push(transport_enum(
        "ProjectionTransportSnapshot",
        "projection",
        true,
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
        false,
        &projections
            .iter()
            .map(|projection| Ok((projection.name, schema_ref_type_name(projection.patch())?)))
            .collect::<Result<Vec<_>>>()?,
    ));
    lines.push(String::new());
    lines.push(transport_enum(
        "ProjectionTransportMutationResult",
        "mutation",
        true,
        &definition
            .mutations()
            .map(|mutation| Ok((mutation.name, schema_ref_type_name(mutation.result())?)))
            .collect::<Result<Vec<_>>>()?,
    ));
    lines.push(String::new());
    lines.push(transport_enum(
        "ProjectionTransportCommandResult",
        "command",
        true,
        &definition
            .commands()
            .map(|command| Ok((command.name, schema_ref_type_name(command.result())?)))
            .collect::<Result<Vec<_>>>()?,
    ));
    lines.push(String::new());
    lines.push(registry_metadata(definition));
    lines.push(String::new());
    lines.push(collaboration_metadata(definition));
    lines.push(String::new());
    let body = lines.join("\n");

    let mut header: Vec<String> = crate::header_lines(project)
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                "//!".to_owned()
            } else {
                format!("//! {line}")
            }
        })
        .collect();
    header.push(String::new());
    header.push("pub use clerkenwell_schema::{".to_owned());
    header.extend(filled(&SCHEMA_REEXPORTS, "    ", 100));
    header.push("};".to_owned());
    header.push("use serde::{Deserialize, Serialize};".to_owned());
    // A model with no `json` field or record type imports neither.
    if body.contains("JsonValue") {
        header.push("use serde_json::Value as JsonValue;".to_owned());
    }
    if body.contains("BTreeMap<") {
        header.push("use std::collections::BTreeMap;".to_owned());
    }
    header.push(String::new());
    Ok(format!("{}\n{body}", header.join("\n")))
}

/// The transport enum `enum_name`, tagged by `tag`, with a variant per entry;
/// a `transport_value` enum implements `GeneratedTransportValue`.
fn transport_enum(
    enum_name: &str,
    tag: &str,
    transport_value: bool,
    entries: &[(&str, String)],
) -> String {
    let mut lines = vec![
        "#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]".to_owned(),
        format!(
            "#[serde(tag = {}, content = \"value\", deny_unknown_fields)]",
            rust_string(tag)
        ),
        // Variants wrap arbitrary schema-defined payloads, so their sizes vary
        // by design.
        "#[allow(clippy::large_enum_variant)]".to_owned(),
        format!("pub enum {enum_name} {{"),
    ];
    if entries.is_empty() {
        let last = lines.len() - 1;
        lines[last] = format!("pub enum {enum_name} {{}}");
    }
    for (name, type_name) in entries {
        lines.push(rename_attribute("    ", name));
        lines.push(layout::wrap_last_argument(
            &format!("    {}(", pascal_identifier(name)),
            type_name,
            "),",
        ));
    }
    if !entries.is_empty() {
        lines.push("}".to_owned());
    }
    let named = match tag {
        "mutation" => Some(("mutation_name", "MutationName")),
        "command" => Some(("command_name", "CommandName")),
        _ => None,
    };
    if let Some((method, name_enum)) = named {
        lines.extend(
            [
                "",
                &format!("impl {enum_name} {{"),
                &format!("    pub const fn {method}(&self) -> {name_enum} {{"),
                if entries.is_empty() {
                    "        match *self {}"
                } else {
                    "        match self {"
                },
            ]
            .map(str::to_owned),
        );
        for (name, _) in entries {
            let variant = pascal_identifier(name);
            let arm = format!("            Self::{variant}(_) => {name_enum}::{variant},");
            if utf16_len(&arm) <= 100 {
                lines.push(arm);
            } else {
                lines.push(format!("            Self::{variant}(_) => {{"));
                lines.push(format!("                {name_enum}::{variant}"));
                lines.push("            }".to_owned());
            }
        }
        if entries.is_empty() {
            lines.extend(["    }", "}"].map(str::to_owned));
        } else {
            lines.extend(["        }", "    }", "}"].map(str::to_owned));
        }
    }
    if transport_value {
        lines.extend(
            [
                "",
                &format!("impl GeneratedTransportValue for {enum_name} {{"),
                "    fn value(&self) -> serde_json::Result<JsonValue> {",
                if entries.is_empty() {
                    "        match *self {}"
                } else {
                    "        match self {"
                },
            ]
            .map(str::to_owned),
        );
        for (name, _) in entries {
            let variant = pascal_identifier(name);
            lines.push(format!(
                "            Self::{variant}(value) => serde_json::to_value(value),"
            ));
        }
        if entries.is_empty() {
            lines.extend(["    }", "}"].map(str::to_owned));
        } else {
            lines.extend(["        }", "    }", "}"].map(str::to_owned));
        }
    }
    lines.join("\n")
}

fn literal(value: &str) -> Expr {
    Expr::atom(rust_string(value))
}

fn schema_path(name: &str, kind: &str, role: &str) -> Expr {
    Expr::atom(schema_constant_identifier(name, kind, role))
}

/// Each projection's, mutation's and command's schema documents, as constants
/// holding their JSON.
fn schema_constants(definition: &Definition) -> Vec<String> {
    let projections = definition.projections().flat_map(|projection| {
        [
            (projection.name, "PROJECTION", "PARAMS", projection.params()),
            (
                projection.name,
                "PROJECTION",
                "SNAPSHOT",
                projection.snapshot(),
            ),
        ]
    });
    let mutations = definition.mutations().flat_map(|mutation| {
        [
            (mutation.name, "MUTATION", "PARAMS", mutation.params()),
            (mutation.name, "MUTATION", "RESULT", mutation.result()),
        ]
    });
    let commands = definition.commands().flat_map(|command| {
        [
            (command.name, "COMMAND", "PARAMS", command.params()),
            (command.name, "COMMAND", "RESULT", command.result()),
        ]
    });
    projections
        .chain(mutations)
        .chain(commands)
        .map(|(name, kind, role, reference)| {
            layout::item(
                &format!(
                    "pub const {}: &str =",
                    schema_constant_identifier(name, kind, role)
                ),
                &Expr::atom(raw_rust_string(
                    &schema_document(definition, reference).stringify(),
                )),
            )
        })
        .collect()
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

fn command_path(name: &str) -> Expr {
    Expr::atom(name_constant_identifier(name, "COMMAND"))
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

fn registry_metadata(definition: &Definition) -> String {
    let projections = definition
        .projections()
        .map(|projection| {
            spec(
                "GeneratedProjectionSpec",
                vec![
                    ("name", projection_path(projection.name)),
                    ("description", literal(projection.description())),
                    ("depends_on", literals(&projection.depends_on())),
                    (
                        "materialization",
                        materialization_plan(projection.materialization()),
                    ),
                    (
                        "params_schema",
                        schema_path(projection.name, "PROJECTION", "PARAMS"),
                    ),
                    (
                        "snapshot_schema",
                        schema_path(projection.name, "PROJECTION", "SNAPSHOT"),
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
                    ("description", literal(mutation.description())),
                    ("touches", literals(&mutation.touches())),
                    (
                        "params_schema",
                        schema_path(mutation.name, "MUTATION", "PARAMS"),
                    ),
                    (
                        "result_schema",
                        schema_path(mutation.name, "MUTATION", "RESULT"),
                    ),
                ],
            )
        })
        .collect();
    let commands = definition
        .commands()
        .map(|command| {
            spec(
                "GeneratedCommandSpec",
                vec![
                    ("name", command_path(command.name)),
                    ("description", literal(command.description())),
                    ("touches", literals(&command.touches())),
                    (
                        "params_schema",
                        schema_path(command.name, "COMMAND", "PARAMS"),
                    ),
                    (
                        "result_schema",
                        schema_path(command.name, "COMMAND", "RESULT"),
                    ),
                ],
            )
        })
        .collect();
    let entities = definition
        .entities()
        .map(|entity| {
            let touching: Vec<&str> = definition
                .mutations()
                .filter(|mutation| mutation.touches().contains(&entity.name))
                .map(|mutation| mutation.name)
                .collect();
            spec(
                "GeneratedEntityAuthoringSpec",
                vec![
                    ("entity", literal(entity.name)),
                    (
                        "kind",
                        Expr::atom(format!(
                            "GeneratedAuthoringPolicyKind::{}",
                            pascal_identifier(entity.authoring_kind())
                        )),
                    ),
                    ("mutations", mutation_paths(&touching)),
                    (
                        "planning_mutations",
                        mutation_paths(&entity.planning_mutations()),
                    ),
                ],
            )
        })
        .collect();
    let mut lines = schema_constants(definition);
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend([
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
            "pub static GENERATED_COMMAND_SPECS: &[GeneratedCommandSpec] =",
            &Expr::slice(commands),
        ),
        String::new(),
        layout::item(
            "pub static GENERATED_ENTITY_AUTHORING_SPECS: &[GeneratedEntityAuthoringSpec] =",
            &Expr::slice(entities),
        ),
    ]);
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
    let mut lines = Vec::new();
    lines.push(format!(
        "pub const COLLABORATION_DEFINITION_VERSION: u32 = {};",
        number_to_string(collaboration.version())
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
                            "required_in_parent",
                            Expr::atom(field.required_in_parent().to_string()),
                        ),
                        (
                            "conflict",
                            Expr::atom(format!(
                                "GeneratedCollaborationConflict::{}",
                                pascal_identifier(field.conflict())
                            )),
                        ),
                        (
                            "writers",
                            Expr::slice(
                                field
                                    .writers()
                                    .into_iter()
                                    .map(|(kind, conflict)| {
                                        spec(
                                            "GeneratedWriterConflict",
                                            vec![
                                                (
                                                    "kind",
                                                    Expr::atom(format!(
                                                        "ActorKind::{}",
                                                        pascal_identifier(kind)
                                                    )),
                                                ),
                                                (
                                                    "conflict",
                                                    Expr::atom(format!(
                                                        "GeneratedCollaborationConflict::{}",
                                                        pascal_identifier(conflict)
                                                    )),
                                                ),
                                            ],
                                        )
                                    })
                                    .collect(),
                            ),
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
                (
                    "schema_version",
                    Expr::atom(number_to_string(entity.schema_version())),
                ),
                (
                    "authoring_projection",
                    projection_path(entity.authoring_projection()),
                ),
                (
                    "authoring_document",
                    Expr::atom(match entity.authoring_document() {
                        Some(document) => format!("Some({})", rust_string(document)),
                        None => "None".to_owned(),
                    }),
                ),
                (
                    "authoring_store_params",
                    literals(&entity.authoring_store_params()),
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
/// A string enum; a value's description is its variant's documentation, as
/// the derived JSON Schema carries it.
fn render_enum(name: &str, values: &[String], descriptions: Option<&Object>) -> String {
    let variants: Vec<String> = values
        .iter()
        .map(|value| {
            let documentation = descriptions
                .and_then(|descriptions| descriptions.get(value))
                .and_then(Json::as_str)
                .map(|description| doc_comment("    ", description))
                .unwrap_or_default();
            format!(
                "{documentation}{}\n    {},",
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
fn render_definition(
    definition: &Definition,
    definition_name: &str,
    schema: &Object,
) -> Result<String> {
    let type_name = pascal_identifier(definition_name);
    if has_type(schema, "json") {
        return Ok(format!("pub type {type_name} = JsonValue;"));
    }
    if is_externally_tagged(schema) {
        return externally_tagged_union(definition, definition_name, schema);
    }
    if matches!(schema.get("$ref"), Some(Json::String(_))) {
        return Ok(format!(
            "pub type {type_name} = {};",
            schema_ref_type_name(schema)?
        ));
    }
    // A scalar union naming two or more types besides `null` is an untagged
    // enum, `null` its `Null` variant.
    if let Some(members) = primitive_union_types(schema)?
        .filter(|members| members.iter().filter(|member| **member != "null").count() > 1)
    {
        return Ok(with_constraints(
            definition,
            primitive_union(definition_name, &members)?,
            schema,
        ));
    }
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        return Ok(with_constraints(
            definition,
            render_enum(
                &type_name,
                &values
                    .iter()
                    .map(|value| value.as_str().unwrap().to_owned())
                    .collect::<Vec<_>>(),
                object_at(schema, "enumDescriptions"),
            ),
            schema,
        ));
    }
    // A nullable schema, a scalar and an array name the Rust type of their
    // values; each use of the alias carries the schema's constraints.
    if renders_as_alias(schema)? {
        let target = rust_type(
            definition,
            definition_name,
            definition_name,
            "value",
            schema,
            false,
        )?;
        return Ok(type_alias(&type_name, &target));
    }
    if !has_type(schema, "object") {
        return refuse(format!(
            "Rust generation currently expects $defs.{definition_name} to be an object schema."
        ));
    }
    if object_at(schema, "oneOf").is_some() {
        return tagged_union(definition, definition_name, schema);
    }
    // A record-shaped `$def` names one open map, so a value schema used at many
    // sites is written once. Its `RecordConstraints` names its values'
    // constraints.
    if let Some(additional) = record_values_schema(schema) {
        let value = rust_type(
            definition,
            definition_name,
            definition_name,
            "value",
            additional,
            false,
        )?;
        let record_constraints = rust_record_constraints_name(definition_name);
        let value_constraints = layout::item_at(
            4,
            "const VALUE_CONSTRAINTS: &'static [SchemaConstraint] =",
            &Expr::slice(
                constraints(definition, additional)
                    .iter()
                    .map(Constraint::expr)
                    .collect(),
            ),
        );
        return Ok(format!(
            "{}\n\npub enum {record_constraints} {{}}\n\nimpl RecordConstraints for {record_constraints} {{\n{value_constraints}\n}}",
            type_alias(
                &type_name,
                &format!("RecordMembers<{record_constraints}, {value}>")
            )
        ));
    }
    render_object(
        TypeOwner {
            definition,
            definition_name,
            enum_owner: definition_name,
        },
        &type_name,
        schema,
        &[],
    )
}

/// An object schema as a struct named `type_name`: closed, or open with its
/// other members flattened beside its declared ones. `also_declared` names
/// members a value of the struct carries without the struct declaring them,
/// such as a union's tag.
fn render_object(
    owner: TypeOwner,
    type_name: &str,
    schema: &Object,
    also_declared: &[&str],
) -> Result<String> {
    let TypeOwner {
        definition,
        definition_name,
        enum_owner,
    } = owner;
    let empty = Object::new();
    let properties = object_at(schema, "properties").unwrap_or(&empty);
    let required = read_string_array(schema.get("required"), &format!("{type_name}.required"))?;
    let mut fields = Vec::new();
    for (property_name, property_schema) in properties.iter() {
        let property_schema = property_schema
            .as_object()
            .expect("validation requires object property schemas");
        fields.push(render_field(
            owner,
            property_name,
            property_schema,
            required.contains(&property_name),
            "    ",
            "pub ",
        )?);
    }
    let derive = "#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]";
    if let Some(additional) = object_at(schema, "additionalProperties") {
        // An open object's other members are flattened beside its declared
        // ones, which serde's `deny_unknown_fields` does not admit.
        // `OtherMembers` holds its values in a map, an indirection.
        let value = rust_type(
            definition,
            definition_name,
            enum_owner,
            OTHER_MEMBERS_FIELD,
            additional,
            true,
        )?;
        fields.push(format!(
            "    #[serde(flatten)]\n{}",
            layout::declaration(
                "    ",
                &format!("pub {OTHER_MEMBERS_FIELD}:"),
                &format!("OtherMembers<{type_name}, {value}>"),
            )
        ));
        let declared: Vec<&str> = also_declared
            .iter()
            .copied()
            .chain(properties.keys())
            .collect();
        let mut items = vec![layout::item_at(
            4,
            "const DECLARED_MEMBERS: &'static [&'static str] =",
            &literals(&declared),
        )];
        let constraints: Vec<Expr> = constraints(definition, additional)
            .iter()
            .map(Constraint::expr)
            .collect();
        if !constraints.is_empty() {
            items.push(layout::item_at(
                4,
                "const OTHER_MEMBER_CONSTRAINTS: &'static [SchemaConstraint] =",
                &Expr::slice(constraints),
            ));
        }
        return Ok(format!(
            "{derive}\npub struct {type_name} {{\n{}\n}}\n\nimpl DeclaredMembers for {type_name} {{\n{}\n}}",
            fields.join("\n"),
            items.join("\n")
        ));
    }
    let derive = format!("{derive}\n#[serde(deny_unknown_fields)]");
    if fields.is_empty() {
        // Unit-like params still reject unknown fields on deserialisation.
        return Ok(format!("{derive}\npub struct {type_name} {{}}"));
    }
    Ok(format!(
        "{derive}\npub struct {type_name} {{\n{}\n}}",
        fields.join("\n")
    ))
}

/// A validation keyword or default within a schema, as a `SchemaConstraint`
/// writes it.
struct Constraint {
    /// The JSON Pointer, within the schema, of the schema it belongs to.
    path: String,
    keyword: &'static str,
    /// Its `SchemaConstraintValue`.
    value: String,
}

impl Constraint {
    /// The `SchemaConstraint` struct expression.
    fn expr(&self) -> Expr {
        Expr::Struct(
            "SchemaConstraint".to_owned(),
            vec![
                ("path".to_owned(), literal(&self.path)),
                ("keyword".to_owned(), literal(self.keyword)),
                ("value".to_owned(), Expr::atom(&self.value)),
            ],
        )
    }

    /// A `schemars` attribute transforming the schema derived for the item it
    /// annotates. `schemars` parses a transform as a call, not as a struct
    /// expression.
    fn attribute(&self, indent: &str) -> String {
        format!(
            "{indent}#[schemars(transform = SchemaConstraint::new({}, {}, {}))]",
            rust_string(&self.path),
            rust_string(self.keyword),
            self.value
        )
    }
}

/// The validation keywords and defaults of `schema` and of the array items,
/// record values and nullable members within it. A reference carries its
/// `default`; a referenced enum, union or struct carries its own
/// constraints, and an alias carries none, so a reference to an alias carries
/// the validation keywords of the schema the alias names.
fn constraints(definition: &Definition, schema: &Object) -> Vec<Constraint> {
    let mut found = Vec::new();
    collect_constraints(definition, schema, String::new(), &mut found);
    found
}

fn collect_constraints(
    definition: &Definition,
    schema: &Object,
    path: String,
    found: &mut Vec<Constraint>,
) {
    let reference = schema.get("$ref").is_some();
    if reference {
        if let Some(target) = alias_target(definition, schema) {
            let mut validation = Vec::new();
            collect_constraints(definition, target, path.clone(), &mut validation);
            found.extend(
                validation
                    .into_iter()
                    .filter(|constraint| constraint.keyword != "default"),
            );
        }
    }
    if let Some(member) = nullable_member(schema) {
        collect_constraints(definition, member, path.clone(), found);
    }
    for keyword in crate::validate::VALIDATION_SCHEMA_KEYS
        .iter()
        .chain(&["default"])
        .filter(|keyword| !reference || **keyword == "default")
    {
        let Some(value) = schema.get(keyword) else {
            continue;
        };
        let value = match value {
            Json::Bool(value) => format!("SchemaConstraintValue::Boolean({value})"),
            Json::String(value) => {
                format!("SchemaConstraintValue::String({})", rust_string(value))
            }
            Json::Null | Json::Array(_) | Json::Object(_) => format!(
                "SchemaConstraintValue::Json({})",
                rust_string(&value.stringify())
            ),
            _ => {
                let number = value.as_f64().expect("validation keeps scalar constraints");
                match i64_literal(number) {
                    Some(integer) => format!("SchemaConstraintValue::Integer({integer})"),
                    None => format!("SchemaConstraintValue::Number({number:?})"),
                }
            }
        };
        found.push(Constraint {
            path: path.clone(),
            keyword,
            value,
        });
    }
    let nested = if has_type(schema, "array") {
        object_at(schema, "items").map(|items| ("items", items))
    } else {
        record_values_schema(schema).map(|values| ("additionalProperties", values))
    };
    if let Some((keyword, nested)) = nested {
        collect_constraints(definition, nested, format!("{path}/{keyword}"), found);
    }
}

/// The schema a reference names when that schema renders as a Rust alias,
/// following references to the end.
fn alias_target<'d>(definition: &'d Definition, reference: &Object) -> Option<&'d Object> {
    let name = crate::schema::local_reference_name(reference.get("$ref"))?;
    let target = definition.def(name)?;
    if target.get("$ref").is_some() {
        return alias_target(definition, target).or(Some(target));
    }
    renders_as_alias(target).ok()?.then_some(target)
}

/// Whether a `$defs` entry renders as a Rust type alias: a nullable schema
/// other than a scalar union, `null`, a scalar, or an array.
fn renders_as_alias(schema: &Object) -> Result<bool> {
    if is_externally_tagged(schema) || schema.get("enum").is_some() {
        return Ok(false);
    }
    if let Some(members) = primitive_union_types(schema)? {
        return Ok(members.iter().filter(|member| **member != "null").count() == 1);
    }
    if nullable_inner(schema).is_some() {
        return Ok(true);
    }
    Ok(matches!(
        schema.get("type").and_then(Json::as_str),
        Some("null" | "string" | "boolean" | "integer" | "number" | "array")
    ))
}

/// A `$defs` enum or union with attributes carrying its validation keywords
/// and default, so the schema derived for each use of the type carries them.
fn with_constraints(definition: &Definition, rendered: String, schema: &Object) -> String {
    let attributes: String = constraints(definition, schema)
        .iter()
        .map(|constraint| format!("{}\n", constraint.attribute("")))
        .collect();
    rendered.replacen("\npub enum ", &format!("\n{attributes}pub enum "), 1)
}

/// `pub type name = target;`, the target on the next line when the line is
/// too wide.
fn type_alias(name: &str, target: &str) -> String {
    let line = format!("pub type {name} = {target};");
    if layout::fits(&line) {
        line
    } else {
        format!("pub type {name} =\n    {target};")
    }
}

fn primitive_union(definition_name: &str, members: &[&str]) -> Result<String> {
    let mut variants = Vec::new();
    for member in members {
        let rust_type = match *member {
            "string" => "String",
            "boolean" => "bool",
            "integer" => "i64",
            "number" => "f64",
            "null" => {
                // An untagged unit variant is written and read as `null`.
                variants.push("    Null,".to_owned());
                continue;
            }
            other => {
                return refuse(format!(
                    "Cannot render unsupported Rust primitive union member {}.",
                    rust_string(other)
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
/// carry no visibility modifier. An open variant is a newtype variant over a
/// struct of its own, named after the union and its tag, which declares the
/// tag among its members.
fn tagged_union(definition: &Definition, definition_name: &str, schema: &Object) -> Result<String> {
    let discriminator = schema
        .get("discriminator")
        .and_then(Json::as_str)
        .expect("validation requires a discriminator");
    let variants = object_at(schema, "oneOf").expect("checked by the caller");
    let mut rendered = Vec::new();
    let mut structs = Vec::new();
    for (tag, variant) in variants.iter() {
        let variant = variant
            .as_object()
            .expect("validation requires object variants");
        let owner = TypeOwner {
            definition,
            definition_name,
            enum_owner: &variant_enum_owner(definition_name, tag),
        };
        if object_at(variant, "additionalProperties").is_some() {
            let struct_name = variant_enum_owner(definition_name, tag);
            structs.push(render_object(
                owner,
                &struct_name,
                variant,
                &[discriminator],
            )?);
            rendered.push(format!(
                "{}\n    {}({struct_name}),",
                rename_attribute("    ", tag),
                pascal_identifier(tag)
            ));
            continue;
        }
        rendered.push(format!(
            "{}\n    {} {{{}}},",
            rename_attribute("    ", tag),
            pascal_identifier(tag),
            struct_variant_body(owner, definition_name, tag, variant)?
        ));
    }
    let mut lines = [
        "#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]"
            .to_owned(),
        format!(
            "#[serde(tag = {}, deny_unknown_fields)]",
            rust_string(discriminator)
        ),
        format!("pub enum {} {{", pascal_identifier(definition_name)),
        rendered.join("\n"),
        "}".to_owned(),
    ]
    .join("\n");
    for rendered_struct in structs {
        lines.push_str("\n\n");
        lines.push_str(&rendered_struct);
    }
    Ok(lines)
}

/// The braced body of a struct variant: its fields, inline when rustfmt
/// would inline them.
fn struct_variant_body(
    owner: TypeOwner,
    definition_name: &str,
    tag: &str,
    variant: &Object,
) -> Result<String> {
    let empty = Object::new();
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
            owner,
            property_name,
            property_schema,
            is_required,
            "        ",
            "",
        )?);
        inline_fields.push(format!(
            "{}: {}",
            rust_field_identifier(property_name),
            field_type(owner, property_name, property_schema, is_required)?
        ));
    }
    let fields = fields.join("\n");
    let inline_body = inline_fields.join(", ");
    // rustfmt inlines a struct variant whose body fits
    // `struct_variant_width` (35) and carries no attributes.
    let inline = !fields.contains("#[") && utf16_len(&inline_body) <= 35;
    Ok(if fields.is_empty() {
        String::new()
    } else if inline {
        format!(" {inline_body} ")
    } else {
        format!("\n{fields}\n    ")
    })
}

/// A `$defs` externally tagged union as a serde enum in its default
/// representation: a data variant is an object of one member named by its
/// tag, a unit variant its tag. A data variant whose value is a closed object
/// is a struct variant, one whose value is an open object a newtype variant
/// over a struct of its own named after the union and its tag, and any other
/// a newtype variant over its value's type.
fn externally_tagged_union(
    definition: &Definition,
    definition_name: &str,
    schema: &Object,
) -> Result<String> {
    let variants = object_at(schema, "oneOf").expect("checked by the caller");
    let descriptions = object_at(schema, "enumDescriptions");
    let mut rendered = Vec::new();
    let mut structs = Vec::new();
    let mut struct_variants = false;
    for (tag, variant) in variants.iter() {
        let variant = variant
            .as_object()
            .expect("validation requires object variants");
        let variant_name = pascal_identifier(tag);
        let rename = rename_attribute("    ", tag);
        let struct_name = variant_enum_owner(definition_name, tag);
        let owner = TypeOwner {
            definition,
            definition_name,
            enum_owner: &struct_name,
        };
        let inline_object =
            has_type(variant, "object") && object_at(variant, "properties").is_some();
        if inline_object && other_members_schema(variant).is_some() {
            structs.push(render_object(owner, &struct_name, variant, &[])?);
            rendered.push(format!("{rename}\n    {variant_name}({struct_name}),"));
        } else if inline_object {
            struct_variants = true;
            rendered.push(format!(
                "{rename}\n    {variant_name} {{{}}},",
                struct_variant_body(owner, definition_name, tag, variant)?
            ));
        } else {
            let value = rust_type(
                definition,
                definition_name,
                definition_name,
                tag,
                variant,
                false,
            )?;
            rendered.push(format!(
                "{rename}\n{}",
                layout::wrap_last_argument(&format!("    {variant_name}("), &value, "),")
            ));
        }
    }
    for unit in schema
        .get("enum")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(Json::as_str)
    {
        let documentation = descriptions
            .and_then(|descriptions| descriptions.get(unit))
            .and_then(Json::as_str)
            .map(|description| doc_comment("    ", description))
            .unwrap_or_default();
        rendered.push(format!(
            "{documentation}{}\n    {},",
            rename_attribute("    ", unit),
            pascal_identifier(unit)
        ));
    }
    let mut lines = vec![
        "#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]"
            .to_owned(),
    ];
    if struct_variants {
        lines.push("#[serde(deny_unknown_fields)]".to_owned());
    }
    lines.extend([
        format!("pub enum {} {{", pascal_identifier(definition_name)),
        rendered.join("\n"),
        "}".to_owned(),
    ]);
    let mut lines = lines.join("\n");
    for rendered_struct in structs {
        lines.push_str("\n\n");
        lines.push_str(&rendered_struct);
    }
    Ok(lines)
}

/// The `$defs` entry a field belongs to, and what its string enums are named
/// after.
#[derive(Clone, Copy)]
struct TypeOwner<'a> {
    definition: &'a Definition,
    definition_name: &'a str,
    enum_owner: &'a str,
}

/// One field, with serde attributes for a renamed or optional wire property.
fn render_field(
    owner: TypeOwner,
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
    if !required && admits_null(owner.definition, property_schema) {
        // An optional member whose values include `null` is absent (`None`) or
        // one of its values (`Some`), `null` among them, as the TypeScript
        // `?:` field is.
        attributes.push(format!("{indent}#[serde(default)]"));
        attributes.push(format!(
            "{indent}#[serde(deserialize_with = \"clerkenwell_schema::present\")]"
        ));
    }
    if !required {
        // `Option<T>` omitted when absent, matching the TypeScript `?` field.
        attributes.push(format!(
            "{indent}#[serde(skip_serializing_if = \"Option::is_none\")]"
        ));
    }
    attributes.extend(
        constraints(owner.definition, property_schema)
            .iter()
            .map(|constraint| constraint.attribute(indent)),
    );
    let field_type = field_type(owner, property_name, property_schema, required)?;
    let prefix: String = attributes
        .iter()
        .map(|attribute| format!("{attribute}\n"))
        .collect();
    Ok(format!(
        "{prefix}{}",
        layout::declaration(indent, &format!("{visibility}{rust_name}:"), &field_type)
    ))
}

/// Whether `null` is a value of `schema`: a nullable schema, `null`, any JSON
/// value, a scalar union naming `null`, or a reference to one.
fn admits_null(definition: &Definition, schema: &Object) -> bool {
    if let Some(name) = crate::schema::local_reference_name(schema.get("$ref")) {
        return definition
            .def(name)
            .is_some_and(|target| admits_null(definition, target));
    }
    nullable_inner(schema).is_some() || has_type(schema, "null") || has_type(schema, "json")
}

/// The Rust type of a member: its value's type, an `Option` of it when the
/// member is optional.
fn field_type(
    owner: TypeOwner,
    property_name: &str,
    property_schema: &Object,
    required: bool,
) -> Result<String> {
    let base = rust_type(
        owner.definition,
        owner.definition_name,
        owner.enum_owner,
        property_name,
        property_schema,
        false,
    )?;
    Ok(if required {
        base
    } else {
        format!("Option<{base}>")
    })
}

/// `/// text` lines for documentation, each line of `text` a line of its own.
fn doc_comment(indent: &str, text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                format!("{indent}///\n")
            } else {
                format!("{indent}/// {}\n", line.trim_end())
            }
        })
        .collect()
}

/// `#[serde(rename = "value")]`, its argument on a line of its own when the
/// attribute is too wide, as rustfmt breaks it.
fn rename_attribute(indent: &str, value: &str) -> String {
    let argument = format!("rename = {}", rust_string(value));
    let line = format!("{indent}#[serde({argument})]");
    if layout::fits_attribute(&line) {
        line
    } else {
        format!("{indent}#[serde(\n{indent}    {argument}\n{indent})]")
    }
}

/// `value` as a Rust string literal.
fn rust_string(value: &str) -> String {
    format!("{value:?}")
}

/// `value` as a raw string literal, with one more `#` than any run of `#`
/// after a `"` in it.
fn raw_rust_string(value: &str) -> String {
    let longest = value
        .split('"')
        .skip(1)
        .map(|after| after.bytes().take_while(|byte| *byte == b'#').count())
        .max()
        .unwrap_or(0);
    let hashes = "#".repeat(longest + 1);
    format!("r{hashes}\"{value}\"{hashes}")
}

/// `value` as the `i64` it is exactly, if it is one.
fn i64_literal(value: f64) -> Option<i64> {
    (value.fract() == 0.0 && value >= i64::MIN as f64 && value < -(i64::MIN as f64))
        .then_some(value as i64)
}

/// Whether a value of `schema` holds a value of `target`'s type without an
/// indirection: through references, struct fields and tagged-union variant
/// fields, but not `Vec`, `BTreeMap`, `RecordMembers` or `OtherMembers`. Such
/// a field is boxed.
fn reaches_directly(
    definition: &Definition,
    schema: &Object,
    target: &str,
    visited: &mut HashSet<String>,
) -> Result<bool> {
    if schema.get("$ref").is_some() {
        let name = schema_ref_name(definition, schema, "Rust type")?;
        if name == target {
            return Ok(true);
        }
        if !visited.insert(name.clone()) {
            return Ok(false);
        }
        let referenced = definition
            .def(&name)
            .expect("schema_ref_name checked it exists");
        return reaches_directly(definition, referenced, target, visited);
    }
    // `Option<T>` holds a `T` directly.
    if let Some(inner) = nullable_inner(schema) {
        return reaches_directly(definition, &inner, target, visited);
    }
    let variants = object_at(schema, "oneOf")
        .into_iter()
        .flat_map(|variants| variants.iter().map(|(_, variant)| variant))
        .filter_map(Json::as_object);
    if is_externally_tagged(schema) {
        // A newtype or struct variant holds its value directly.
        for variant in variants {
            if reaches_directly(definition, variant, target, visited)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if !has_type(schema, "object") {
        return Ok(false);
    }
    let members = object_at(schema, "properties")
        .into_iter()
        .chain(variants.filter_map(|variant| object_at(variant, "properties")))
        .flat_map(|properties| properties.iter().map(|(_, member)| member))
        .filter_map(Json::as_object);
    for member in members {
        if reaches_directly(definition, member, target, visited)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The Rust type of a field of `definition_name`. A field holding its own
/// type directly is boxed; `Vec` and `BTreeMap` already are indirections. A
/// string enum is the helper enum named after `enum_owner` and the property.
fn rust_type(
    definition: &Definition,
    definition_name: &str,
    enum_owner: &str,
    property_name: &str,
    schema: &Object,
    behind_indirection: bool,
) -> Result<String> {
    if matches!(schema.get("$ref"), Some(Json::String(_))) {
        let referenced = schema_ref_type_name(schema)?;
        let recursive = !behind_indirection
            && reaches_directly(definition, schema, definition_name, &mut HashSet::new())?;
        return Ok(if recursive {
            format!("Box<{referenced}>")
        } else {
            referenced
        });
    }
    if schema.get("enum").and_then(Json::as_array).is_some() {
        return Ok(rust_property_enum_name(enum_owner, property_name));
    }
    let scalar_union = primitive_union_types(schema)?
        .is_some_and(|members| members.iter().filter(|member| **member != "null").count() > 1);
    if scalar_union {
        return refuse(format!(
            "Rust generation does not support anonymous primitive unions at {definition_name}.{property_name}. Use a $defs reference."
        ));
    }
    if let Some(inner) = nullable_inner(schema) {
        return Ok(format!(
            "Option<{}>",
            rust_type(
                definition,
                definition_name,
                enum_owner,
                property_name,
                &inner,
                behind_indirection,
            )?
        ));
    }
    let schema_type = schema.get("type");
    Ok(match schema_type.and_then(Json::as_str) {
        Some("array") => {
            let items = object_at(schema, "items").expect("validation requires array items");
            format!(
                "Vec<{}>",
                rust_type(definition, definition_name, enum_owner, property_name, items, true)?
            )
        }
        Some("string") => "String".to_owned(),
        Some("boolean") => "bool".to_owned(),
        Some("integer") => "i64".to_owned(),
        Some("number") => "f64".to_owned(),
        Some("json") => "JsonValue".to_owned(),
        Some("null") => "()".to_owned(),
        Some("object") => match record_values_schema(schema) {
            Some(additional) => format!(
                "BTreeMap<String, {}>",
                rust_type(
                    definition,
                    definition_name,
                    enum_owner,
                    property_name,
                    additional,
                    true
                )?
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
    let literal = rust_string(value);
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
    if values.is_empty() {
        return [
            "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]".to_owned(),
            format!("pub enum {enum_name} {{}}"),
            String::new(),
            format!("impl {enum_name} {{"),
            "    pub const fn as_str(self) -> &'static str {".to_owned(),
            "        match self {}".to_owned(),
            "    }".to_owned(),
            String::new(),
            "    pub fn parse(_value: &str) -> Option<Self> {".to_owned(),
            "        None".to_owned(),
            "    }".to_owned(),
            "}".to_owned(),
        ]
        .join("\n");
    }
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
