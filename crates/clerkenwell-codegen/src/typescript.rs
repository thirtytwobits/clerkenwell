//! The TypeScript model module.

use crate::config::Project;
use crate::definition::Definition;
use crate::error::{refuse, Result};
use crate::json::{number_to_string, string_literal, Json, Object};
use crate::names::{name_constant_identifier, pascal_identifier};
use crate::replica::optional_paths;
use crate::schema::{
    object_at, other_members_schema, primitive_union_types, read_string_array,
    record_values_schema, schema_ref_type_name, stringify_or_undefined,
};

/// The generated-file header as a block comment, without a trailing newline.
fn module_header(project: &Project) -> String {
    let mut lines = vec!["/**".to_owned()];
    lines.extend(crate::header_lines(project).into_iter().map(|line| {
        if line.is_empty() {
            " *".to_owned()
        } else {
            format!(" * {line}")
        }
    }));
    lines.push(" */".to_owned());
    lines.join("\n")
}

/// The `@clerkenwell/client` types the module imports for its own
/// declarations, each with the name it binds.
const CLIENT_TYPE_IMPORTS: [(&str, &str); 9] = [
    ("AuthoringPeerBlock", "AuthoringPeerBlock"),
    ("AuthoringPlan", "AuthoringPlan"),
    ("AuthoringState", "AuthoringState"),
    ("AuthoringStatePatch", "AuthoringStatePatch"),
    (
        "CollaborationEntityPlan",
        "ClerkenwellCollaborationEntityPlan",
    ),
    ("CollaborationExchangeMode", "CollaborationExchangeMode"),
    (
        "CollaborationPlanTextFieldPath",
        "CollaborationPlanTextFieldPath",
    ),
    (
        "CollaborationReplicaDocument",
        "CollaborationReplicaDocument",
    ),
    (
        "ProjectionCompositionPlan",
        "ClerkenwellProjectionCompositionPlan",
    ),
];

/// The `@clerkenwell/client` types the module exports as its own.
const CLIENT_TYPE_EXPORTS: [&str; 4] = [
    "AuthoringPolicyKind",
    "CollaborationFieldPlan",
    "CollaborationStorageKind",
    "CollaborationValueCodec",
];

/// The `@clerkenwell/client` value the module exports as its own.
const CLIENT_VALUE_EXPORT: &str = "resolveCollaborationContainer";

/// Every name the module binds from `@clerkenwell/client`, which no `$defs`
/// entry may generate.
pub(crate) fn client_bindings() -> impl Iterator<Item = &'static str> {
    CLIENT_TYPE_IMPORTS
        .iter()
        .map(|(_, bound)| *bound)
        .chain(CLIENT_TYPE_EXPORTS)
        .chain([CLIENT_VALUE_EXPORT])
}

/// `items` as one per line between `open` and `close`, separated by commas.
fn listed(open: &str, items: &[String], close: &str) -> Vec<String> {
    let mut lines = vec![open.to_owned()];
    for (index, item) in items.iter().enumerate() {
        let separator = if index + 1 < items.len() { "," } else { "" };
        lines.push(format!("  {item}{separator}"));
    }
    lines.push(close.to_owned());
    lines
}

/// The React-free TypeScript model module.
pub fn render_model_module(definition: &Definition, project: &Project) -> Result<String> {
    let mut lines = vec![module_header(project), String::new()];
    // The authoring states' types are `@clerkenwell/client`'s: the module
    // imports them for its own declarations and exports them as its own.
    let from_client = "} from \"@clerkenwell/client\";";
    let imports: Vec<String> = CLIENT_TYPE_IMPORTS
        .iter()
        .map(|(name, bound)| {
            if name == bound {
                (*name).to_owned()
            } else {
                format!("{name} as {bound}")
            }
        })
        .collect();
    lines.extend(listed("import type {", &imports, from_client));
    let exports: Vec<String> = CLIENT_TYPE_EXPORTS.map(str::to_owned).to_vec();
    lines.extend(listed("export type {", &exports, from_client));
    let mut shared: Vec<String> = crate::expand::SHARED.map(str::to_owned).to_vec();
    shared.sort();
    lines.extend(listed("export type {", &shared, "};"));
    lines.push(format!(
        "export {{ {CLIENT_VALUE_EXPORT} }} from \"@clerkenwell/client\";"
    ));
    lines.push(String::new());
    for (definition_name, schema) in definition.declared_def_entries() {
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
    let command_names: Vec<&str> = definition.commands().map(|command| command.name).collect();
    // Readonly tuples give consumers runtime iteration and a literal union.
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
    lines.push(readonly_tuple("ENTITY_NAMES", &entity_names));
    lines.push("export type EntityName = typeof ENTITY_NAMES[number];".to_owned());
    lines.push(String::new());
    lines.push(readonly_tuple("PROJECTION_NAMES", &projection_names));
    lines.push("export type ProjectionName = typeof PROJECTION_NAMES[number];".to_owned());
    lines.push(String::new());
    lines.push(readonly_tuple("MUTATION_NAMES", &mutation_names));
    lines.push("export type MutationName = typeof MUTATION_NAMES[number];".to_owned());
    lines.push(String::new());
    lines.push(readonly_tuple("COMMAND_NAMES", &command_names));
    lines.push("export type CommandName = typeof COMMAND_NAMES[number];".to_owned());
    lines.push(String::new());
    lines.push(contract_interfaces(definition)?);
    lines.push(String::new());
    lines.extend(
        [
            "/** The projection model @clerkenwell/client is generic over. */",
            "export type GeneratedProjectionModel = {",
            "  projections: {",
            "    [K in ProjectionName]: {",
            "      params: ProjectionParamsByName[K];",
            "      snapshot: ProjectionSnapshotByName[K];",
            "      patch: ProjectionPatchByName[K];",
            "    };",
            "  };",
            "  mutations: {",
            "    [K in MutationName]: {",
            "      params: MutationParamsByName[K];",
            "      result: MutationResultByName[K];",
            "    };",
            "  };",
            "  commands: {",
            "    [K in CommandName]: {",
            "      params: CommandParamsByName[K];",
            "      result: CommandResultByName[K];",
            "    };",
            "  };",
            "};",
            "",
        ]
        .map(str::to_owned),
    );
    lines.push(composition_metadata(definition)?);
    lines.push(String::new());
    Ok(format!("{}\n", lines.join("\n")))
}

/// Maps keyed by projection and mutation name, so client APIs keep each
/// name's params, snapshot, patch and result types.
fn contract_interfaces(definition: &Definition) -> Result<String> {
    let mut lines = Vec::new();
    let interface =
        |title: &str, entries: Vec<(&str, &Object)>, lines: &mut Vec<String>| -> Result<()> {
            if entries.is_empty() {
                lines.push(format!("export interface {title} {{}}"));
                return Ok(());
            }
            lines.push(format!("export interface {title} {{"));
            for (name, reference) in entries {
                lines.push(format!(
                    "  {}: {};",
                    string_literal(name),
                    schema_ref_type_name(reference)?
                ));
            }
            lines.push("}".to_owned());
            Ok(())
        };
    let projections: Vec<_> = definition.projections().collect();
    let mutations: Vec<_> = definition.mutations().collect();
    let commands: Vec<_> = definition.commands().collect();
    interface(
        "ProjectionParamsByName",
        projections.iter().map(|p| (p.name, p.params())).collect(),
        &mut lines,
    )?;
    lines.push(String::new());
    interface(
        "ProjectionSnapshotByName",
        projections.iter().map(|p| (p.name, p.snapshot())).collect(),
        &mut lines,
    )?;
    lines.push(String::new());
    interface(
        "ProjectionPatchByName",
        projections.iter().map(|p| (p.name, p.patch())).collect(),
        &mut lines,
    )?;
    lines.push(String::new());
    interface(
        "MutationParamsByName",
        mutations.iter().map(|m| (m.name, m.params())).collect(),
        &mut lines,
    )?;
    lines.push(String::new());
    interface(
        "MutationResultByName",
        mutations.iter().map(|m| (m.name, m.result())).collect(),
        &mut lines,
    )?;
    lines.push(String::new());
    interface(
        "CommandParamsByName",
        commands.iter().map(|c| (c.name, c.params())).collect(),
        &mut lines,
    )?;
    lines.push(String::new());
    interface(
        "CommandResultByName",
        commands.iter().map(|c| (c.name, c.result())).collect(),
        &mut lines,
    )?;
    lines.extend(
        [
            "",
            "export type ProjectionTransportSnapshot = {",
            "  [K in ProjectionName]: { projection: K; value: ProjectionSnapshotByName[K] };",
            "}[ProjectionName];",
            "",
            "export type ProjectionTransportPatch = {",
            "  [K in ProjectionName]: { projection: K; value: ProjectionPatchByName[K] };",
            "}[ProjectionName];",
            "",
            "export type ProjectionTransportMutationResult = {",
            "  [K in MutationName]: { mutation: K; value: MutationResultByName[K] };",
            "}[MutationName];",
            "",
            "export type ProjectionTransportCommandResult = {",
            "  [K in CommandName]: { command: K; value: CommandResultByName[K] };",
            "}[CommandName];",
        ]
        .map(str::to_owned),
    );
    Ok(lines.join("\n"))
}

fn composition_metadata(definition: &Definition) -> Result<String> {
    let plans: Object = definition
        .projections()
        .map(|projection| {
            let mut plan = projection.materialization().raw.clone();
            plan.insert("dependsOn", projection.depends_on().into());
            (projection.name, Json::from(plan))
        })
        .collect();
    let effects: Object = definition
        .mutations()
        .map(|mutation| (mutation.name, Json::from(mutation.touches())))
        .collect();
    let command_effects: Object = definition
        .commands()
        .map(|command| (command.name, Json::from(command.touches())))
        .collect();
    let authoring_plans: Object = definition
        .entities()
        .map(|entity| {
            let mutations: Vec<&str> = definition
                .mutations()
                .filter(|mutation| mutation.touches().contains(&entity.name))
                .map(|mutation| mutation.name)
                .collect();
            let mut plan = Object::new();
            plan.insert("kind", entity.authoring_kind().into());
            plan.insert("schemaVersion", definition.version().into());
            plan.insert("revision", entity.revision().clone().into());
            plan.insert("mutations", mutations.into());
            plan.insert("planningMutations", entity.planning_mutations().into());
            (entity.name, Json::from(plan))
        })
        .collect();
    Ok([
        "export type ProjectionCompositionPlan = ClerkenwellProjectionCompositionPlan<EntityName>;".to_owned(),
        String::new(),
        format!(
            "export const PROJECTION_COMPOSITION_PLANS = {} as const satisfies Record<ProjectionName, ProjectionCompositionPlan>;",
            Json::from(plans).stringify_pretty()
        ),
        String::new(),
        format!(
            "export const MUTATION_EFFECT_PLANS = {} as const satisfies Record<MutationName, readonly EntityName[]>;",
            Json::from(effects).stringify_pretty()
        ),
        String::new(),
        format!(
            "export const COMMAND_EFFECT_PLANS = {} as const satisfies Record<CommandName, readonly EntityName[]>;",
            Json::from(command_effects).stringify_pretty()
        ),
        String::new(),
        "export type GeneratedAuthoringPlan = AuthoringPlan<MutationName>;".to_owned(),
        format!(
            "export const AUTHORING_PLANS = {} as const satisfies Record<EntityName, GeneratedAuthoringPlan>;",
            Json::from(authoring_plans).stringify_pretty()
        ),
        String::new(),
        collaboration_metadata(definition)?,
    ]
    .join("\n"))
}

fn collaboration_metadata(definition: &Definition) -> Result<String> {
    let collaboration = definition.collaboration();
    let plans: Object = collaboration
        .entities()
        .map(|entity| {
            let fields: Object = entity
                .fields()
                .map(|field| {
                    let mut plan = Object::new();
                    plan.insert("path", field.path.into());
                    plan.spread(field.raw);
                    plan.insert("requiredInParent", field.required_in_parent().into());
                    (field.path, Json::from(plan))
                })
                .collect();
            let mut plan = Object::new();
            plan.insert("schemaVersion", entity.schema_version().into());
            plan.insert("authoringState", entity.authoring_state().clone().into());
            plan.insert("rootContainer", entity.root_container().into());
            plan.insert("fields", fields.into());
            (entity.name, Json::from(plan))
        })
        .collect();
    Ok([
        "export type CollaborationEntityPlan = ClerkenwellCollaborationEntityPlan<ProjectionName, MutationName>;".to_owned(),
        format!(
            "export const COLLABORATION_DEFINITION_VERSION = {} as const;",
            number_to_string(collaboration.version())
        ),
        format!(
            "export const COLLABORATION_PLANS = {} as const satisfies Record<string, CollaborationEntityPlan>;",
            Json::from(plans).stringify_pretty()
        ),
        "export type CollaborationEntityName = keyof typeof COLLABORATION_PLANS;".to_owned(),
        "export type CollaborationFieldPath<TEntity extends CollaborationEntityName> = keyof (typeof COLLABORATION_PLANS)[TEntity][\"fields\"] & string;".to_owned(),
        "export type CollaborationTextFieldPath<TEntity extends CollaborationEntityName> = CollaborationPlanTextFieldPath<(typeof COLLABORATION_PLANS)[TEntity]>;".to_owned(),
        collaboration_documents(definition)?,
    ]
    .join("\n"))
}

/// Each collaborative entity's document as a replica holds it: its document
/// type, with the members a replica may leave out optional.
fn collaboration_documents(definition: &Definition) -> Result<String> {
    let mut lines = vec![
        String::new(),
        "/** Each collaborative entity's document as a replica holds it. */".to_owned(),
        "export type CollaborationDocument<TEntity extends CollaborationEntityName> = {".to_owned(),
    ];
    for entity in definition.collaboration().entities() {
        let schema = definition
            .entity(entity.name)
            .expect("validation requires the collaborative entity")
            .schema();
        let mut arguments = vec![schema_ref_type_name(schema)?];
        let optional = optional_paths(definition, entity)?;
        if !optional.is_empty() {
            arguments.push(
                optional
                    .iter()
                    .map(|path| string_literal(path))
                    .collect::<Vec<_>>()
                    .join(" | "),
            );
        }
        lines.push(format!(
            "  {}: CollaborationReplicaDocument<{}>;",
            string_literal(entity.name),
            arguments.join(", ")
        ));
    }
    lines.push("}[TEntity];".to_owned());
    Ok(lines.join("\n"))
}

/// `export type Name = ...;` with no trailing whitespace on any line.
fn render_definition(definition_name: &str, schema: &Object) -> Result<String> {
    let rendered = format!(
        "export type {} = {};",
        pascal_identifier(definition_name),
        schema_to_typescript(schema)?
    );
    let segments: Vec<&str> = rendered.split('\n').collect();
    let last = segments.len() - 1;
    Ok(segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            if index == last {
                *segment
            } else {
                segment.trim_end_matches([' ', '\t'])
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// A TypeScript type expression for the schema subset. Property names are
/// quoted to keep the wire names exactly.
fn schema_to_typescript(schema: &Object) -> Result<String> {
    if matches!(schema.get("$ref"), Some(Json::String(_))) {
        return schema_ref_type_name(schema);
    }
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        return Ok(values
            .iter()
            .map(Json::stringify)
            .collect::<Vec<_>>()
            .join(" | "));
    }
    if let Some(variants) = object_at(schema, "oneOf") {
        let discriminator = schema
            .get("discriminator")
            .and_then(Json::as_str)
            .expect("validation requires a discriminator");
        let mut rendered_variants = String::new();
        for (tag, variant) in variants.iter() {
            let variant = variant
                .as_object()
                .expect("validation requires object variants");
            let rendered = schema_to_typescript(variant)?;
            let tag_field = format!(
                "  {}: {};",
                string_literal(discriminator),
                string_literal(tag)
            );
            if rendered == "{}" {
                rendered_variants.push_str(&format!("\n  | {{\n{tag_field}\n}}"));
            } else {
                let body = &rendered[2..rendered.len() - 2];
                rendered_variants.push_str(&format!("\n  | {{\n{tag_field}\n{body}\n}}"));
            }
        }
        return Ok(rendered_variants);
    }
    if let Some(members) = primitive_union_types(schema)? {
        let mut types: Vec<&str> = Vec::new();
        for member in members {
            let rendered = if member == "integer" {
                "number"
            } else {
                member
            };
            if !types.contains(&rendered) {
                types.push(rendered);
            }
        }
        return Ok(types.join(" | "));
    }
    let schema_type = schema.get("type");
    match schema_type.and_then(Json::as_str) {
        Some("object") => {
            if let Some(additional) = record_values_schema(schema) {
                return Ok(format!(
                    "Record<string, {}>",
                    schema_to_typescript(additional)?
                ));
            }
            let Some(properties) = object_at(schema, "properties") else {
                return refuse(
                    "Cannot render an object schema without properties as a TypeScript type.",
                );
            };
            if properties.is_empty() {
                // An empty params object is an exact type of its own.
                return Ok("{}".to_owned());
            }
            let required = read_string_array(schema.get("required"), "schema.required")?;
            let mut fields = Vec::new();
            for (property_name, property_schema) in properties.iter() {
                let optional = if required.contains(&property_name) {
                    ""
                } else {
                    "?"
                };
                let property_schema = property_schema
                    .as_object()
                    .expect("validation requires object property schemas");
                fields.push(format!(
                    "  {}{optional}: {};",
                    string_literal(property_name),
                    schema_to_typescript(property_schema)?
                ));
            }
            // An other member may be `undefined`, as an optional declared
            // member may: JSON leaves out both.
            if let Some(additional) = other_members_schema(schema) {
                fields.push(format!(
                    "  [member: string]: {} | undefined;",
                    schema_to_typescript(additional)?
                ));
            }
            Ok(format!("{{\n{}\n}}", fields.join("\n")))
        }
        Some("array") => {
            let items = object_at(schema, "items").expect("validation requires array items");
            let item_type = schema_to_typescript(items)?;
            Ok(if item_type.contains(" | ") {
                format!("({item_type})[]")
            } else {
                format!("{item_type}[]")
            })
        }
        Some("string") => Ok("string".to_owned()),
        Some("boolean") => Ok("boolean".to_owned()),
        Some("integer" | "number") => Ok("number".to_owned()),
        Some("json") => Ok("unknown".to_owned()),
        _ => refuse(format!(
            "Cannot render unsupported TypeScript schema type {}.",
            stringify_or_undefined(schema_type)
        )),
    }
}

fn name_constants(suffix: &str, values: &[&str]) -> Vec<String> {
    values
        .iter()
        .map(|value| {
            format!(
                "export const {} = {};",
                name_constant_identifier(value, suffix),
                string_literal(value)
            )
        })
        .collect()
}

fn readonly_tuple(name: &str, values: &[&str]) -> String {
    if values.is_empty() {
        return format!("export const {name} = [] as const;");
    }
    let rendered: Vec<String> = values
        .iter()
        .map(|value| format!("  {},", string_literal(value)))
        .collect();
    format!(
        "export const {name} = [\n{}\n] as const;",
        rendered.join("\n")
    )
}
