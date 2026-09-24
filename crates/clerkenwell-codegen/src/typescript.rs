//! The TypeScript model module and the mnemonic module.

use crate::config::Project;
use crate::definition::{Definition, Fields};
use crate::error::{refuse, Result};
use crate::json::{number_to_string, string_literal, Json, Object};
use crate::names::{constant_name, name_constant_identifier, object_key, pascal_identifier};
use crate::schema::{
    object_at, primitive_union_types, read_string_array, resolve_ref, schema_ref_name,
    schema_ref_type_name, stringify_or_undefined,
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
    let mnemonic_keys: Object = definition
        .mnemonics()
        .map(|mnemonic| (mnemonic.name, Json::from(mnemonic.key())))
        .collect();
    lines.push(readonly_object("MNEMONIC_KEYS", &mnemonic_keys));
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
    lines.push(composition_metadata(definition, project));
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

fn composition_metadata(definition: &Definition, project: &Project) -> String {
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
    let session_key = project
        .authoring_session_mnemonic
        .as_deref()
        .and_then(|name| definition.mnemonic(name))
        .map(|mnemonic| Json::from(mnemonic.key()));
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
            plan.insert("rationale", entity.rationale().into());
            plan.insert("revision", entity.revision().clone().into());
            plan.insert("mutations", mutations.into());
            plan.insert_some("contentMutation", entity.content_mutation().map(Json::from));
            plan.insert("planningMutations", entity.planning_mutations().into());
            plan.insert("commandMutations", entity.command_mutations().into());
            plan.insert("lifecycleMutations", entity.lifecycle_mutations().into());
            let session = if entity.owns_session() {
                let mut session = Object::new();
                session.insert_some("mnemonicKey", session_key.clone());
                session.insert("leavePolicy", "durableRestoreOrConfirmDiscard".into());
                session.insert(
                    "conflictPolicy",
                    if entity.authoring_kind() == "collaborative" {
                        "generatedFieldPolicy"
                    } else {
                        "expectedRevision"
                    }
                    .into(),
                );
                session.into()
            } else {
                Json::Null
            };
            plan.insert("authoringSession", session);
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
                    (field.path, Json::from(plan))
                })
                .collect();
            let mut plan = Object::new();
            plan.insert("substrate", entity.substrate().into());
            plan.insert("schemaVersion", entity.schema_version().into());
            plan.insert("migrationIds", entity.migration_ids().into());
            plan.insert("authoringState", entity.authoring_state().clone().into());
            plan.insert("rootContainer", entity.root_container().into());
            plan.insert("clientPathNaming", entity.client_path_naming().into());
            plan.insert("fields", fields.into());
            (entity.name, Json::from(plan))
        })
        .collect();
    let compatibility = collaboration.compatibility();
    [
        "export type CollaborationEntityPlan = ClerkenwellCollaborationEntityPlan<ProjectionName, MutationName>;".to_owned(),
        format!(
            "export const COLLABORATION_DEFINITION_VERSION = {} as const;",
            number_to_string(collaboration.version())
        ),
        format!(
            "export const COLLABORATION_MINIMUM_READER_VERSION = {} as const;",
            number_to_string(compatibility.number_field("minimumReaderVersion"))
        ),
        format!(
            "export const COLLABORATION_MINIMUM_WRITER_VERSION = {} as const;",
            number_to_string(compatibility.number_field("minimumWriterVersion"))
        ),
        format!(
            "export const COLLABORATION_PLANS = {} as const satisfies Record<string, CollaborationEntityPlan>;",
            Json::from(plans).stringify_pretty()
        ),
        "export type CollaborationEntityName = keyof typeof COLLABORATION_PLANS;".to_owned(),
        "export type CollaborationFieldPath<TEntity extends CollaborationEntityName> = keyof (typeof COLLABORATION_PLANS)[TEntity][\"fields\"] & string;".to_owned(),
        "export type CollaborationTextFieldPath<TEntity extends CollaborationEntityName> = {".to_owned(),
        "  [TPath in CollaborationFieldPath<TEntity>]: (typeof COLLABORATION_PLANS)[TEntity][\"fields\"][TPath] extends { readonly storage: { readonly kind: \"text\" } } ? TPath : never;".to_owned(),
        "}[CollaborationFieldPath<TEntity>];".to_owned(),
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

/// The mnemonic module: each entry's plan, key and standalone JSON Schema.
pub fn render_mnemonic_module(
    definition: &Definition,
    project: &Project,
    model_specifier: &str,
) -> Result<String> {
    let mut type_imports: Vec<String> = Vec::new();
    for mnemonic in definition.mnemonics() {
        let type_name = schema_ref_type_name(mnemonic.schema())?;
        if !type_imports.contains(&type_name) {
            type_imports.push(type_name);
        }
    }
    let mut lines = vec![
        module_header(project),
        String::new(),
        format!(
            "import type {{ {} }} from {};",
            type_imports.join(", "),
            string_literal(model_specifier)
        ),
        String::new(),
    ];

    lines.push("export const MNEMONIC_PLANS = {".to_owned());
    for mnemonic in definition.mnemonics() {
        lines.push(format!("  {}: {{", object_key(mnemonic.name)));
        lines.push(format!("    key: {},", string_literal(mnemonic.key())));
        lines.push(format!(
            "    version: {},",
            number_to_string(mnemonic.version())
        ));
        lines.push(format!(
            "    cachePolicy: {},",
            string_literal(mnemonic.cache_policy())
        ));
        lines.push(format!(
            "    migrationIds: {},",
            Json::from(mnemonic.migration_ids()).stringify()
        ));
        lines.push(format!(
            "    recovery: {}",
            string_literal(mnemonic.recovery())
        ));
        lines.push("  },".to_owned());
    }
    lines.push("} as const;".to_owned());
    lines.push(String::new());

    // Plain JSON Schema data rather than a persistence library's builder calls:
    // the model is a contract, independent of how a client persists.
    for mnemonic in definition.mnemonics() {
        let definition_name = schema_ref_name(
            definition,
            mnemonic.schema(),
            &format!("mnemonic.{}.schema", mnemonic.name),
        )?;
        let schema_name = pascal_identifier(&definition_name);
        let prefix = constant_name(mnemonic.name);
        let schema = definition.def(&definition_name).expect("resolved above");
        lines.push(format!(
            "export const {prefix}_MNEMONIC_KEY = {};",
            string_literal(mnemonic.key())
        ));
        lines.push(String::new());
        lines.push(format!("export const {prefix}_MNEMONIC_DEFINITION = {{"));
        lines.push(format!("  key: {prefix}_MNEMONIC_KEY,"));
        lines.push(format!(
            "  version: {},",
            number_to_string(mnemonic.version())
        ));
        lines.push(format!(
            "  schema: {}",
            crate::names::js_trim_start(&json_schema(definition, schema, 2)?)
        ));
        lines.push("} as const;".to_owned());
        lines.push(String::new());
        lines.push(format!(
            "export type {schema_name}MnemonicValue = {schema_name};"
        ));
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

/// The schema subset as plain JSON Schema with references inlined, so each
/// emitted schema stands alone.
fn json_schema(definition: &Definition, schema: &Object, indent: usize) -> Result<String> {
    let pad = " ".repeat(indent);
    if matches!(schema.get("$ref"), Some(Json::String(_))) {
        return json_schema(
            definition,
            resolve_ref(definition, schema, "mnemonic schema")?,
            indent,
        );
    }
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        let values: Vec<String> = values.iter().map(Json::stringify).collect();
        return Ok(format!(
            "{pad}{{ \"type\": \"string\", \"enum\": [{}] }}",
            values.join(", ")
        ));
    }
    if let Some(members) = primitive_union_types(schema)? {
        let members: Vec<String> = members.into_iter().map(string_literal).collect();
        return Ok(format!("{pad}{{ \"type\": [{}] }}", members.join(", ")));
    }
    let schema_type = schema.get("type");
    let trim = crate::names::js_trim_start;
    match schema_type.and_then(Json::as_str) {
        Some("object") => {
            if let Some(additional) = object_at(schema, "additionalProperties") {
                let rendered = json_schema(definition, additional, indent + 2)?;
                return Ok([
                    format!("{pad}{{"),
                    format!("{pad}  \"type\": \"object\","),
                    format!("{pad}  \"additionalProperties\": {}", trim(&rendered)),
                    format!("{pad}}}"),
                ]
                .join("\n"));
            }
            if object_at(schema, "oneOf").is_some() {
                return refuse("Cannot render a tagged union as a mnemonic schema.");
            }
            let Some(properties) = object_at(schema, "properties") else {
                return refuse(
                    "Cannot render an object schema without properties as a mnemonic schema.",
                );
            };
            if properties.is_empty() {
                return Ok(format!(
                    "{pad}{{ \"type\": \"object\", \"properties\": {{}}, \"additionalProperties\": false }}"
                ));
            }
            let required = read_string_array(schema.get("required"), "schema.required")?;
            let mut property_lines = Vec::new();
            for (property_name, property_schema) in properties.iter() {
                let property_schema = property_schema
                    .as_object()
                    .expect("validation requires object property schemas");
                let rendered = json_schema(definition, property_schema, indent + 6)?;
                property_lines.push(format!(
                    "{}{}: {}",
                    " ".repeat(indent + 4),
                    string_literal(property_name),
                    trim(&rendered)
                ));
            }
            let required_names: Vec<String> = required.into_iter().map(string_literal).collect();
            Ok([
                format!("{pad}{{"),
                format!("{pad}  \"type\": \"object\","),
                format!("{pad}  \"properties\": {{"),
                property_lines.join(",\n"),
                format!("{pad}  }},"),
                format!("{pad}  \"required\": [{}],", required_names.join(", ")),
                format!("{pad}  \"additionalProperties\": false"),
                format!("{pad}}}"),
            ]
            .join("\n"))
        }
        Some("array") => {
            let items = object_at(schema, "items").expect("validation requires array items");
            let rendered = json_schema(definition, items, indent + 2)?;
            Ok([
                format!("{pad}{{"),
                format!("{pad}  \"type\": \"array\","),
                format!("{pad}  \"items\": {}", trim(&rendered)),
                format!("{pad}}}"),
            ]
            .join("\n"))
        }
        Some(primitive @ ("string" | "boolean" | "integer" | "number")) => Ok(format!(
            "{pad}{{ \"type\": {} }}",
            string_literal(primitive)
        )),
        // Any JSON is valid; the value type comes from the generated alias.
        Some("json") => Ok(format!("{pad}{{}}")),
        _ => refuse(format!(
            "Cannot render unsupported mnemonic schema type {}.",
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

fn readonly_object(name: &str, entries: &Object) -> String {
    let rendered: Vec<String> = entries
        .iter()
        .map(|(key, value)| format!("  {}: {},", string_literal(key), value.stringify()))
        .collect();
    format!(
        "export const {name} = {{\n{}\n}} as const;",
        rendered.join("\n")
    )
}
