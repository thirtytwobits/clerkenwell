//! The TypeScript model module.

use crate::config::Project;
use crate::definition::Definition;
use crate::error::{refuse, Result};
use crate::json::{number_to_string, string_literal, Json, Object};
use crate::names::{name_constant_identifier, pascal_identifier};
use crate::schema::{
    object_at, primitive_union_types, read_string_array, schema_ref_type_name,
    stringify_or_undefined,
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

/// The React-free TypeScript model module.
pub fn render_model_module(definition: &Definition, project: &Project) -> Result<String> {
    let mut lines = vec![module_header(project), String::new()];
    lines.extend(
        [
            "import type {",
            "  AuthoringPlan,",
            "  CollaborationEntityPlan as ClerkenwellCollaborationEntityPlan,",
            "  CollaborationPlanTextFieldPath,",
            "  ProjectionCompositionPlan as ClerkenwellProjectionCompositionPlan",
            "} from \"@clerkenwell/client\";",
            "export type {",
            "  AuthoringPolicyKind,",
            "  CollaborationFieldPlan,",
            "  CollaborationStorageKind,",
            "  CollaborationValueCodec",
            "} from \"@clerkenwell/client\";",
            "export { resolveCollaborationContainer } from \"@clerkenwell/client\";",
            "",
        ]
        .map(str::to_owned),
    );
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
    // Readonly tuples give consumers runtime iteration and a literal union.
    lines.extend(name_constants("ENTITY", &entity_names));
    lines.push(String::new());
    lines.extend(name_constants("PROJECTION", &projection_names));
    lines.push(String::new());
    lines.extend(name_constants("MUTATION", &mutation_names));
    lines.push(String::new());
    lines.push(readonly_tuple("ENTITY_NAMES", &entity_names));
    lines.push("export type EntityName = typeof ENTITY_NAMES[number];".to_owned());
    lines.push(String::new());
    lines.push(readonly_tuple("PROJECTION_NAMES", &projection_names));
    lines.push("export type ProjectionName = typeof PROJECTION_NAMES[number];".to_owned());
    lines.push(String::new());
    lines.push(readonly_tuple("MUTATION_NAMES", &mutation_names));
    lines.push("export type MutationName = typeof MUTATION_NAMES[number];".to_owned());
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
            "};",
            "",
        ]
        .map(str::to_owned),
    );
    lines.push(composition_metadata(definition));
    lines.push(String::new());
    Ok(format!("{}\n", lines.join("\n")))
}

/// Maps keyed by projection and mutation name, so client APIs keep each
/// name's params, snapshot, patch and result types.
fn contract_interfaces(definition: &Definition) -> Result<String> {
    let mut lines = Vec::new();
    let interface =
        |title: &str, entries: Vec<(&str, &Object)>, lines: &mut Vec<String>| -> Result<()> {
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
        ]
        .map(str::to_owned),
    );
    Ok(lines.join("\n"))
}

fn composition_metadata(definition: &Definition) -> String {
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
    [
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
        "export type GeneratedAuthoringPlan = AuthoringPlan<MutationName>;".to_owned(),
        format!(
            "export const AUTHORING_PLANS = {} as const satisfies Record<EntityName, GeneratedAuthoringPlan>;",
            Json::from(authoring_plans).stringify_pretty()
        ),
        String::new(),
        collaboration_metadata(definition),
    ]
    .join("\n")
}

fn collaboration_metadata(definition: &Definition) -> String {
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
            plan.insert("migrationIds", entity.migration_ids().into());
            plan.insert("authoringState", entity.authoring_state().clone().into());
            plan.insert("rootContainer", entity.root_container().into());
            plan.insert("fields", fields.into());
            (entity.name, Json::from(plan))
        })
        .collect();
    [
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
    ]
    .join("\n")
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
            if let Some(additional) = object_at(schema, "additionalProperties") {
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
    let rendered: Vec<String> = values
        .iter()
        .map(|value| format!("  {},", string_literal(value)))
        .collect();
    format!(
        "export const {name} = [\n{}\n] as const;",
        rendered.join("\n")
    )
}
