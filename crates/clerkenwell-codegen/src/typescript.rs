//! The TypeScript model module.

use crate::client::{client_name, client_type_name, ClientSpelling};
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
    // The authoring states' types are `@clerkenwell/client`'s: the module
    // imports them for its own declarations and exports them as its own.
    lines.extend(
        [
            "import type {",
            "  AuthoringPeerBlock,",
            "  AuthoringPlan,",
            "  AuthoringState,",
            "  AuthoringStatePatch,",
            "  CollaborationEntityPlan as ClerkenwellCollaborationEntityPlan,",
            "  CollaborationExchangeMode,",
            "  CollaborationPlanTextFieldPath,",
            "  ProjectionCompositionPlan as ClerkenwellProjectionCompositionPlan",
            "} from \"@clerkenwell/client\";",
            "export type {",
            "  AuthoringPolicyKind,",
            "  CollaborationFieldPlan,",
            "  CollaborationStorageKind,",
            "  CollaborationValueCodec",
            "} from \"@clerkenwell/client\";",
            "export type {",
            "  AuthoringPeerBlock,",
            "  AuthoringState,",
            "  AuthoringStatePatch,",
            "  CollaborationExchangeMode",
            "};",
            "export { resolveCollaborationContainer } from \"@clerkenwell/client\";",
            "",
        ]
        .map(str::to_owned),
    );
    for (definition_name, schema) in definition.declared_def_entries() {
        lines.push(render_definition(definition_name, schema)?);
        lines.push(String::new());
    }
    let spelling = ClientSpelling::of(definition)?;
    for definition_name in spelling.types() {
        lines.push(render_client_definition(
            definition,
            &spelling,
            definition_name,
        )?);
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
    lines.push(composition_metadata(definition, &spelling)?);
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

fn composition_metadata(definition: &Definition, spelling: &ClientSpelling) -> Result<String> {
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
        "export type GeneratedAuthoringPlan = AuthoringPlan<MutationName>;".to_owned(),
        format!(
            "export const AUTHORING_PLANS = {} as const satisfies Record<EntityName, GeneratedAuthoringPlan>;",
            Json::from(authoring_plans).stringify_pretty()
        ),
        String::new(),
        collaboration_metadata(definition, spelling)?,
    ]
    .join("\n"))
}

fn collaboration_metadata(definition: &Definition, spelling: &ClientSpelling) -> Result<String> {
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
        client_documents(definition, spelling)?,
    ]
    .join("\n"))
}

/// Each collaborative entity's document as a TypeScript replica holds it.
fn client_documents(definition: &Definition, spelling: &ClientSpelling) -> Result<String> {
    let mut lines = vec![
        String::new(),
        "/** Each collaborative entity's document as a TypeScript replica holds it. */".to_owned(),
        "export type CollaborationClientDocument<TEntity extends CollaborationEntityName> = {"
            .to_owned(),
    ];
    for entity in definition.collaboration().entities() {
        let schema = definition
            .entity(entity.name)
            .expect("validation requires the collaborative entity")
            .schema();
        lines.push(format!(
            "  {}: {};",
            string_literal(entity.name),
            spelling.reference(schema)?
        ));
    }
    lines.push("}[TEntity];".to_owned());
    Ok(lines.join("\n"))
}

/// `export type Name = ...;` in wire spelling.
fn render_definition(definition_name: &str, schema: &Object) -> Result<String> {
    Ok(declaration(
        &pascal_identifier(definition_name),
        &schema_to_typescript(schema, None)?,
    ))
}

/// The client document type of `definition_name`.
fn render_client_definition(
    definition: &Definition,
    spelling: &ClientSpelling,
    definition_name: &str,
) -> Result<String> {
    let name = client_type_name(definition_name);
    if spelling.is_unchanged(definition_name) {
        return Ok(declaration(&name, &pascal_identifier(definition_name)));
    }
    let schema = definition
        .def(definition_name)
        .expect("a client document type is of a $defs entry");
    let at = ClientAt {
        spelling,
        definition_name,
        path: String::new(),
    };
    Ok(declaration(
        &name,
        &schema_to_typescript(schema, Some(&at))?,
    ))
}

/// `export type Name = type;` with no trailing whitespace on any line.
fn declaration(name: &str, rendered_type: &str) -> String {
    let rendered = format!("export type {name} = {rendered_type};");
    let segments: Vec<&str> = rendered.split('\n').collect();
    let last = segments.len() - 1;
    segments
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
        .join("\n")
}

/// A schema node rendered in client spelling: the `$defs` entry holding it
/// and its path within the entry.
struct ClientAt<'a> {
    spelling: &'a ClientSpelling,
    definition_name: &'a str,
    path: String,
}

impl ClientAt<'_> {
    fn child(&self, segment: &str) -> Self {
        ClientAt {
            spelling: self.spelling,
            definition_name: self.definition_name,
            path: if self.path.is_empty() {
                segment.to_owned()
            } else {
                format!("{}.{segment}", self.path)
            },
        }
    }
}

/// A TypeScript type expression for the schema subset, in wire spelling or,
/// with `client`, in client spelling. Property names are quoted to keep each
/// spelling exactly.
fn schema_to_typescript(schema: &Object, client: Option<&ClientAt>) -> Result<String> {
    if matches!(schema.get("$ref"), Some(Json::String(_))) {
        return match client {
            Some(at) => at.spelling.reference(schema),
            None => schema_ref_type_name(schema),
        };
    }
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        return Ok(values
            .iter()
            .map(Json::stringify)
            .collect::<Vec<_>>()
            .join(" | "));
    }
    let member = |name: &str| match client {
        Some(_) => client_name(name),
        None => name.to_owned(),
    };
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
            let variant_at = client.map(|at| at.child(tag));
            let rendered = schema_to_typescript(variant, variant_at.as_ref())?;
            let tag_field = format!(
                "  {}: {};",
                string_literal(&member(discriminator)),
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
                let value_at = client.map(|at| at.child("*"));
                return Ok(format!(
                    "Record<string, {}>",
                    schema_to_typescript(additional, value_at.as_ref())?
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
                let property_at = client.map(|at| at.child(property_name));
                let held_in_wire_spelling = property_at
                    .as_ref()
                    .is_some_and(|at| at.spelling.is_wire(at.definition_name, &at.path));
                let left_out = property_at
                    .as_ref()
                    .is_some_and(|at| at.spelling.is_optional(at.definition_name, &at.path));
                let optional = if required.contains(&property_name) && !left_out {
                    ""
                } else {
                    "?"
                };
                let property_schema = property_schema
                    .as_object()
                    .expect("validation requires object property schemas");
                let property_at = property_at.filter(|_| !held_in_wire_spelling);
                fields.push(format!(
                    "  {}{optional}: {};",
                    string_literal(&member(property_name)),
                    schema_to_typescript(property_schema, property_at.as_ref())?
                ));
            }
            Ok(format!("{{\n{}\n}}", fields.join("\n")))
        }
        Some("array") => {
            let items = object_at(schema, "items").expect("validation requires array items");
            let items_at = client.map(|at| at.child("*"));
            let item_type = schema_to_typescript(items, items_at.as_ref())?;
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
