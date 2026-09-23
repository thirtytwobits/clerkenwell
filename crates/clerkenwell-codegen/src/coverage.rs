//! The ownership and coverage ledger: who owns every entity, projection,
//! mutation and mnemonic entry, and how much of the wire is described.

use std::collections::{HashMap, HashSet};

use crate::collate::code_unit_compare;
use crate::config::Project;
use crate::definition::Definition;
use crate::error::{refuse, Result};
use crate::json::{Json, Object};
use crate::schema::{object_at, ref_string, schema_ref_name};

/// Every `json` field in the definition, split by whether it says why it is one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PayloadCoverage {
    /// `json` fields with no `opaqueReason`, as `Definition.path`.
    pub undescribed: Vec<String>,
    /// `json` fields with an `opaqueReason`, as `(path, reason)`.
    pub reasoned: Vec<(String, String)>,
    /// Projections whose snapshot reaches an undescribed field.
    pub projections_carrying_undescribed: Vec<String>,
}

/// Collects every `json` field, and the projections that carry an undescribed one.
///
/// A path is the `$defs` name followed by `.property`, `[]` for array items,
/// `{}` for record values and `|tag` for a tagged union variant.
pub fn collect_payload_coverage(definition: &Definition) -> Result<PayloadCoverage> {
    let mut coverage = PayloadCoverage::default();
    let mut references: HashMap<&str, HashSet<String>> = HashMap::new();

    fn walk(
        definition: &Definition,
        coverage: &mut PayloadCoverage,
        references: &mut HashSet<String>,
        definition_name: &str,
        schema: &Json,
        path: &str,
    ) -> Result<()> {
        let Json::Object(schema) = schema else {
            return Ok(());
        };
        if matches!(schema.get("$ref"), Some(Json::String(_))) {
            references.insert(schema_ref_name(definition, schema, definition_name)?);
            return Ok(());
        }
        if schema.get("type").and_then(Json::as_str) == Some("json") {
            let location = format!("{definition_name}{path}");
            match schema.get("opaqueReason").and_then(Json::as_str) {
                Some(reason) => coverage.reasoned.push((location, reason.to_owned())),
                None => coverage.undescribed.push(location),
            }
            return Ok(());
        }
        if let Some(variants) = object_at(schema, "oneOf") {
            for (tag, variant) in variants.iter() {
                walk(
                    definition,
                    coverage,
                    references,
                    definition_name,
                    variant,
                    &format!("{path}|{tag}"),
                )?;
            }
        }
        if let Some(properties) = object_at(schema, "properties") {
            for (name, property) in properties.iter() {
                walk(
                    definition,
                    coverage,
                    references,
                    definition_name,
                    property,
                    &format!("{path}.{name}"),
                )?;
            }
        }
        if let Some(items) = schema
            .get("items")
            .filter(|items| items.as_object().is_some())
        {
            walk(
                definition,
                coverage,
                references,
                definition_name,
                items,
                &format!("{path}[]"),
            )?;
        }
        if let Some(additional) = schema
            .get("additionalProperties")
            .filter(|value| value.as_object().is_some())
        {
            walk(
                definition,
                coverage,
                references,
                definition_name,
                additional,
                &format!("{path}{{}}"),
            )?;
        }
        Ok(())
    }

    for (definition_name, schema) in definition.defs().iter() {
        let mut reached = HashSet::new();
        walk(
            definition,
            &mut coverage,
            &mut reached,
            definition_name,
            schema,
            "",
        )?;
        references.insert(definition_name, reached);
    }

    // A projection carries an undescribed payload when its snapshot reaches
    // one, through however many `$ref` hops.
    let holders: HashSet<&str> = coverage
        .undescribed
        .iter()
        .map(|entry| {
            entry
                .split(['.', '[', '{', '|'])
                .next()
                .expect("split yields a first segment")
        })
        .collect();
    let reaches = |start: &str| -> bool {
        let mut seen = HashSet::new();
        let mut queue = vec![start.to_owned()];
        while let Some(name) = queue.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if holders.contains(name.as_str()) {
                return true;
            }
            if let Some(next) = references.get(name.as_str()) {
                queue.extend(next.iter().cloned());
            }
        }
        false
    };

    let mut carrying = Vec::new();
    for projection in definition.projections() {
        let snapshot = schema_ref_name(
            definition,
            projection.snapshot(),
            &format!("projections.{}.snapshot", projection.name),
        )?;
        if reaches(&snapshot) {
            carrying.push(projection.name.to_owned());
        }
    }
    coverage.projections_carrying_undescribed = carrying;
    Ok(coverage)
}

/// The coverage report. Refuses a definition whose ownership is incomplete.
pub fn render_coverage_report(definition: &Definition, project: &Project) -> Result<String> {
    let mut missing: Vec<String> = Vec::new();
    let mut multiply_owned: Vec<String> = Vec::new();
    let mut mnemonic_owners: HashMap<&str, &str> = HashMap::new();
    let payloads = collect_payload_coverage(definition)?;
    let session_mnemonic = project
        .authoring_session_mnemonic
        .as_deref()
        .and_then(|name| definition.mnemonic(name));
    let collaboration = definition.collaboration();

    for mnemonic in definition.mnemonics() {
        match mnemonic_owners.get(mnemonic.key()) {
            Some(previous) => multiply_owned.push(format!(
                "mnemonic:{}:{previous},{}",
                mnemonic.key(),
                mnemonic.name
            )),
            None => {
                mnemonic_owners.insert(mnemonic.key(), mnemonic.name);
            }
        }
    }

    let mut entities = Vec::new();
    for entity in definition.entities() {
        let name = entity.name;
        let mutations: Vec<&str> = definition
            .mutations()
            .filter(|mutation| mutation.touches().contains(&name))
            .map(|mutation| mutation.name)
            .collect();
        let projections: Vec<&str> = definition
            .projections()
            .filter(|projection| projection.depends_on().contains(&name))
            .map(|projection| projection.name)
            .collect();
        let collaboration_entity = collaboration.entity(name);
        let owns_session = entity.owns_session();
        let mut classified: Vec<&str> = entity
            .content_mutation()
            .into_iter()
            .chain(entity.planning_mutations())
            .chain(entity.command_mutations())
            .chain(entity.lifecycle_mutations())
            .collect();
        classified.sort_by(|left, right| code_unit_compare(left, right));
        let mut touching = mutations.clone();
        touching.sort_by(|left, right| code_unit_compare(left, right));
        if owns_session && classified != touching {
            missing.push(format!("entity:{name}:authoringMutationClassification"));
        }
        if entity.authoring_kind() == "collaborative" && collaboration_entity.is_none() {
            missing.push(format!("entity:{name}:collaboration"));
        }
        if owns_session && session_mnemonic.is_none() {
            missing.push(format!("entity:{name}:authoringSessionMnemonic"));
        }

        let mut ownership = Object::new();
        ownership.insert(
            "rust",
            "ProjectionName/MutationName exhaustive dispatch".into(),
        );
        ownership.insert(
            "typescript",
            "ProjectionSnapshotByName/MutationResultByName mapped unions".into(),
        );
        let materialisers: Vec<Json> = projections
            .iter()
            .map(|projection_name| {
                let projection = definition
                    .projection(projection_name)
                    .expect("listed above");
                let mut materialiser = Object::new();
                materialiser.insert("projection", (*projection_name).into());
                materialiser.insert("strategy", projection.materialization().strategy().into());
                materialiser.into()
            })
            .collect();
        let collaboration_summary = match collaboration_entity {
            None => Json::Null,
            Some(collaboration_entity) => {
                let mut summary = Object::new();
                summary.insert(
                    "schemaVersion",
                    collaboration_entity.schema_version().into(),
                );
                summary.insert("migrationIds", collaboration_entity.migration_ids().into());
                summary.insert(
                    "authoringStateProjection",
                    collaboration_entity.authoring_projection().into(),
                );
                summary.insert(
                    "importMutation",
                    collaboration_entity.import_mutation().into(),
                );
                summary.insert(
                    "fieldCount",
                    (collaboration_entity.field_count() as f64).into(),
                );
                summary.into()
            }
        };
        let diagnostics: Vec<&str> = match project.entity_diagnostics.get(name) {
            Some(configured) => configured.iter().map(String::as_str).collect(),
            None if owns_session => vec!["authoring-runtime-metadata"],
            None => Vec::new(),
        };

        let mut report = Object::new();
        report.insert("entity", name.into());
        report.insert("policy", entity.authoring_kind().into());
        report.insert("rationale", entity.rationale().into());
        report.insert("schema", ref_string(entity.schema()).into());
        report.insert("identityField", entity.id().into());
        report.insert("revision", entity.revision().clone().into());
        report.insert("projections", projections.into());
        report.insert("mutations", mutations.into());
        report.insert(
            "contentMutation",
            entity.content_mutation().map_or(Json::Null, Json::from),
        );
        report.insert("planningMutations", entity.planning_mutations().into());
        report.insert("commandMutations", entity.command_mutations().into());
        report.insert("lifecycleMutations", entity.lifecycle_mutations().into());
        report.insert("generatedHandlerOwnership", ownership.into());
        report.insert("materialisers", Json::Array(materialisers));
        report.insert(
            "authoringSessionMnemonic",
            match (owns_session, session_mnemonic) {
                (true, Some(mnemonic)) => mnemonic.key().into(),
                _ => Json::Null,
            },
        );
        report.insert("collaboration", collaboration_summary);
        report.insert("diagnostics", diagnostics.into());
        entities.push(report.into());
    }

    let projections: Vec<Json> = definition
        .projections()
        .map(|projection| {
            let mut entry = Object::new();
            entry.insert("name", projection.name.into());
            entry.insert("params", ref_string(projection.params()).into());
            entry.insert("snapshot", ref_string(projection.snapshot()).into());
            entry.insert("patch", ref_string(projection.patch()).into());
            entry.insert("dependsOn", projection.depends_on().into());
            entry.insert(
                "materialisation",
                projection.materialization().raw.clone().into(),
            );
            entry.insert(
                "generatedRegistryOwner",
                "GENERATED_PROJECTION_SPECS".into(),
            );
            entry.into()
        })
        .collect();
    let mutations: Vec<Json> = definition
        .mutations()
        .map(|mutation| {
            let mut entry = Object::new();
            entry.insert("name", mutation.name.into());
            entry.insert("params", ref_string(mutation.params()).into());
            entry.insert("result", ref_string(mutation.result()).into());
            entry.insert("touches", mutation.touches().into());
            entry.insert("conflict", mutation.conflict().into());
            entry.insert("generatedRegistryOwner", "GENERATED_MUTATION_SPECS".into());
            entry.into()
        })
        .collect();
    let mnemonics: Vec<Json> = definition
        .mnemonics()
        .map(|mnemonic| {
            let mut entry = Object::new();
            entry.insert("name", mnemonic.name.into());
            entry.insert("key", mnemonic.key().into());
            entry.insert("schema", ref_string(mnemonic.schema()).into());
            entry.insert("version", mnemonic.version().into());
            entry.insert("cachePolicy", mnemonic.cache_policy().into());
            entry.insert("migrationIds", mnemonic.migration_ids().into());
            entry.insert("recovery", mnemonic.recovery().into());
            entry.insert("generatedRegistryOwner", "MNEMONIC_SCHEMAS".into());
            entry.into()
        })
        .collect();

    let mut ownership = Object::new();
    ownership.insert(
        "missing",
        missing
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .into(),
    );
    ownership.insert(
        "multiplyOwned",
        multiply_owned
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .into(),
    );

    // `ownership` asks whether every projection has an owner; `payloads` asks
    // whether what they carry is described.
    let mut payload_section = Object::new();
    payload_section.insert(
        "undescribedCount",
        (payloads.undescribed.len() as f64).into(),
    );
    payload_section.insert("reasonedCount", (payloads.reasoned.len() as f64).into());
    payload_section.insert(
        "projectionsCarryingUndescribed",
        (payloads.projections_carrying_undescribed.len() as f64).into(),
    );
    payload_section.insert(
        "projectionCount",
        (definition.projection_count() as f64).into(),
    );
    payload_section.insert(
        "undescribed",
        payloads
            .undescribed
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .into(),
    );
    payload_section.insert(
        "reasoned",
        Json::Array(
            payloads
                .reasoned
                .iter()
                .map(|(path, reason)| {
                    let mut entry = Object::new();
                    entry.insert("path", path.as_str().into());
                    entry.insert("reason", reason.as_str().into());
                    entry.into()
                })
                .collect(),
        ),
    );

    let mut report = Object::new();
    report.insert("reportVersion", 1.0.into());
    report.insert("definitionVersion", definition.version().into());
    report.insert(
        "collaborationDefinitionVersion",
        collaboration.version().into(),
    );
    report.insert("namespace", definition.namespace().into());
    report.insert("entities", Json::Array(entities));
    report.insert("projections", Json::Array(projections));
    report.insert("mutations", Json::Array(mutations));
    report.insert("mnemonic", Json::Array(mnemonics));
    report.insert("ownership", ownership.into());
    report.insert("payloads", payload_section.into());

    if !missing.is_empty() || !multiply_owned.is_empty() {
        let mut incomplete = missing;
        incomplete.extend(multiply_owned);
        return refuse(format!(
            "Projection coverage is incomplete: {}.",
            incomplete.join(", ")
        ));
    }
    Ok(format!("{}\n", Json::Object(report).stringify_pretty()))
}
