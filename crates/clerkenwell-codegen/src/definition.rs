//! A validated projection definition and typed views over it.
//!
//! The views borrow the parsed document rather than copying it into structs:
//! several artefacts reproduce a section exactly as it was written, key order
//! included, so the document stays the one representation.

use std::path::Path;

use crate::config::Project;
use crate::error::{Error, Result};
use crate::json::{Json, Object};
use crate::validate;

/// A projection definition that has passed the meta-schema and every semantic
/// check.
#[derive(Clone, Debug)]
pub struct Definition {
    document: Object,
}

/// Field access for sections the meta-schema has already shaped.
pub(crate) trait Fields {
    fn field(&self, key: &str) -> Option<&Json>;

    fn str_field(&self, key: &str) -> &str {
        self.opt_str(key)
            .unwrap_or_else(|| panic!("the meta-schema requires string {key:?}"))
    }

    fn opt_str(&self, key: &str) -> Option<&str> {
        self.field(key).and_then(Json::as_str)
    }

    fn object_field(&self, key: &str) -> &Object {
        self.field(key)
            .and_then(Json::as_object)
            .unwrap_or_else(|| panic!("the meta-schema requires object {key:?}"))
    }

    fn number_field(&self, key: &str) -> f64 {
        self.field(key)
            .and_then(Json::as_f64)
            .unwrap_or_else(|| panic!("the meta-schema requires number {key:?}"))
    }

    fn strs_field(&self, key: &str) -> Vec<&str> {
        self.field(key)
            .and_then(Json::as_array)
            .unwrap_or_else(|| panic!("the meta-schema requires array {key:?}"))
            .iter()
            .map(|item| {
                item.as_str()
                    .unwrap_or_else(|| panic!("the meta-schema requires strings in {key:?}"))
            })
            .collect()
    }

    fn opt_strs_field(&self, key: &str) -> Option<Vec<&str>> {
        self.field(key).map(|_| self.strs_field(key))
    }
}

impl Fields for Object {
    fn field(&self, key: &str) -> Option<&Json> {
        self.get(key)
    }
}

impl Definition {
    /// Reads and validates the definition at `path`.
    pub fn load(path: &Path, project: &Project) -> Result<Definition> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        let document = Json::parse(&text).map_err(|source| Error::Parse {
            path: path.to_owned(),
            source,
        })?;
        Definition::from_json(document, project)
    }

    /// Validates `document` against the meta-schema, the semantic rules, and
    /// the names `project` expects it to declare.
    pub fn from_json(document: Json, project: &Project) -> Result<Definition> {
        validate::check_meta_schema(&document)?;
        let Json::Object(document) = document else {
            return Err(Error::Definition(
                "Projection definition must be a JSON object.".to_owned(),
            ));
        };
        let definition = Definition { document };
        validate::check_semantics(&definition, project)?;
        validate::check_project_names(&definition, project)?;
        Ok(definition)
    }

    /// The document as parsed.
    pub fn document(&self) -> &Object {
        &self.document
    }

    pub fn version(&self) -> f64 {
        self.document.number_field("version")
    }

    pub fn compatibility(&self) -> &Object {
        self.document.object_field("compatibility")
    }

    pub fn namespace(&self) -> &str {
        self.document.str_field("namespace")
    }

    fn section(&self, key: &str) -> &Object {
        self.document.object_field(key)
    }

    pub fn entities(&self) -> impl Iterator<Item = Entity<'_>> {
        self.section("entities")
            .iter()
            .map(|(name, raw)| Entity::new(name, raw))
    }

    pub fn entity(&self, name: &str) -> Option<Entity<'_>> {
        self.entities().find(|entity| entity.name == name)
    }

    pub fn entity_names(&self) -> Vec<&str> {
        self.section("entities").keys().collect()
    }

    pub fn projections(&self) -> impl Iterator<Item = Projection<'_>> {
        self.section("projections")
            .iter()
            .map(|(name, raw)| Projection {
                name,
                raw: object(raw, "projection"),
            })
    }

    pub fn projection(&self, name: &str) -> Option<Projection<'_>> {
        self.projections()
            .find(|projection| projection.name == name)
    }

    pub fn projection_count(&self) -> usize {
        self.section("projections").len()
    }

    pub fn mutations(&self) -> impl Iterator<Item = Mutation<'_>> {
        self.section("mutations")
            .iter()
            .map(|(name, raw)| Mutation {
                name,
                raw: object(raw, "mutation"),
            })
    }

    pub fn mutation(&self, name: &str) -> Option<Mutation<'_>> {
        self.mutations().find(|mutation| mutation.name == name)
    }

    pub fn mnemonics(&self) -> impl Iterator<Item = Mnemonic<'_>> {
        self.section("mnemonic").iter().map(|(name, raw)| Mnemonic {
            name,
            raw: object(raw, "mnemonic"),
        })
    }

    pub fn mnemonic(&self, name: &str) -> Option<Mnemonic<'_>> {
        self.mnemonics().find(|mnemonic| mnemonic.name == name)
    }

    pub fn collaboration(&self) -> Collaboration<'_> {
        Collaboration {
            raw: self.section("collaboration"),
        }
    }

    /// The `$defs` section.
    pub fn defs(&self) -> &Object {
        self.section("$defs")
    }

    pub fn def(&self, name: &str) -> Option<&Object> {
        self.defs().get(name).and_then(Json::as_object)
    }

    pub fn def_entries(&self) -> impl Iterator<Item = (&str, &Object)> {
        self.defs()
            .iter()
            .map(|(name, schema)| (name, object(schema, "$defs entry")))
    }
}

fn object<'a>(value: &'a Json, what: &str) -> &'a Object {
    value
        .as_object()
        .unwrap_or_else(|| panic!("the meta-schema requires every {what} to be an object"))
}

/// One `entities` entry.
#[derive(Clone, Copy, Debug)]
pub struct Entity<'a> {
    pub name: &'a str,
    pub raw: &'a Object,
}

impl<'a> Entity<'a> {
    fn new(name: &'a str, raw: &'a Json) -> Self {
        Entity {
            name,
            raw: object(raw, "entity"),
        }
    }

    pub fn id(&self) -> &'a str {
        self.raw.str_field("id")
    }

    /// The `{ "$ref": ... }` naming the entity schema.
    pub fn schema(&self) -> &'a Object {
        self.raw.object_field("schema")
    }

    pub fn revision(&self) -> &'a Object {
        self.raw.object_field("revision")
    }

    pub fn revision_kind(&self) -> &'a str {
        self.revision().str_field("kind")
    }

    pub fn authoring(&self) -> &'a Object {
        self.raw.object_field("authoring")
    }

    pub fn authoring_kind(&self) -> &'a str {
        self.authoring().str_field("kind")
    }

    /// Whether the entity is authored in a session a client keeps.
    pub fn owns_session(&self) -> bool {
        matches!(
            self.authoring_kind(),
            "collaborative" | "optimisticDocument"
        )
    }

    pub fn rationale(&self) -> &'a str {
        self.authoring().str_field("rationale")
    }

    pub fn content_mutation(&self) -> Option<&'a str> {
        self.authoring().opt_str("contentMutation")
    }

    pub fn planning_mutations(&self) -> Vec<&'a str> {
        self.authoring().strs_field("planningMutations")
    }

    pub fn command_mutations(&self) -> Vec<&'a str> {
        self.authoring().strs_field("commandMutations")
    }

    pub fn lifecycle_mutations(&self) -> Vec<&'a str> {
        self.authoring().strs_field("lifecycleMutations")
    }
}

/// One `projections` entry.
#[derive(Clone, Copy, Debug)]
pub struct Projection<'a> {
    pub name: &'a str,
    pub raw: &'a Object,
}

impl<'a> Projection<'a> {
    pub fn params(&self) -> &'a Object {
        self.raw.object_field("params")
    }

    pub fn snapshot(&self) -> &'a Object {
        self.raw.object_field("snapshot")
    }

    pub fn patch(&self) -> &'a Object {
        self.raw.object_field("patch")
    }

    pub fn depends_on(&self) -> Vec<&'a str> {
        self.raw.strs_field("dependsOn")
    }

    pub fn materialization(&self) -> Materialization<'a> {
        Materialization {
            raw: self.raw.object_field("materialization"),
        }
    }
}

/// A projection's `materialization` plan.
#[derive(Clone, Copy, Debug)]
pub struct Materialization<'a> {
    pub raw: &'a Object,
}

impl<'a> Materialization<'a> {
    pub fn strategy(&self) -> &'a str {
        self.raw.str_field("strategy")
    }

    /// A string-valued setting, present or not.
    pub fn setting(&self, key: &str) -> Option<&'a str> {
        self.raw.opt_str(key)
    }

    /// A string-valued setting the meta-schema requires for this strategy.
    pub fn required(&self, key: &str) -> &'a str {
        self.raw.str_field(key)
    }

    pub fn snapshot_omit_fields(&self) -> Option<Vec<&'a str>> {
        self.raw.opt_strs_field("snapshotOmitFields")
    }
}

/// One `mutations` entry.
#[derive(Clone, Copy, Debug)]
pub struct Mutation<'a> {
    pub name: &'a str,
    pub raw: &'a Object,
}

impl<'a> Mutation<'a> {
    pub fn params(&self) -> &'a Object {
        self.raw.object_field("params")
    }

    pub fn result(&self) -> &'a Object {
        self.raw.object_field("result")
    }

    pub fn conflict(&self) -> &'a str {
        self.raw.str_field("conflict")
    }

    pub fn touches(&self) -> Vec<&'a str> {
        self.raw.strs_field("touches")
    }
}

/// One `mnemonic` entry.
#[derive(Clone, Copy, Debug)]
pub struct Mnemonic<'a> {
    pub name: &'a str,
    pub raw: &'a Object,
}

impl<'a> Mnemonic<'a> {
    pub fn key(&self) -> &'a str {
        self.raw.str_field("key")
    }

    pub fn version(&self) -> f64 {
        self.raw.number_field("version")
    }

    pub fn schema(&self) -> &'a Object {
        self.raw.object_field("schema")
    }

    pub fn cache_policy(&self) -> &'a str {
        self.raw.str_field("cachePolicy")
    }

    pub fn migration_ids(&self) -> Vec<&'a str> {
        self.raw.strs_field("migrationIds")
    }

    pub fn recovery(&self) -> &'a str {
        self.raw.str_field("recovery")
    }
}

/// The `collaboration` section.
#[derive(Clone, Copy, Debug)]
pub struct Collaboration<'a> {
    pub raw: &'a Object,
}

impl<'a> Collaboration<'a> {
    pub fn version(&self) -> f64 {
        self.raw.number_field("version")
    }

    pub fn compatibility(&self) -> &'a Object {
        self.raw.object_field("compatibility")
    }

    pub fn entities(&self) -> impl Iterator<Item = CollaborationEntity<'a>> {
        self.raw
            .object_field("entities")
            .iter()
            .map(|(name, raw)| CollaborationEntity {
                name,
                raw: object(raw, "collaboration entity"),
            })
    }

    pub fn entity(&self, name: &str) -> Option<CollaborationEntity<'a>> {
        self.entities().find(|entity| entity.name == name)
    }

    pub fn entity_names(&self) -> Vec<&'a str> {
        self.raw.object_field("entities").keys().collect()
    }
}

/// One `collaboration.entities` entry.
#[derive(Clone, Copy, Debug)]
pub struct CollaborationEntity<'a> {
    pub name: &'a str,
    pub raw: &'a Object,
}

impl<'a> CollaborationEntity<'a> {
    pub fn substrate(&self) -> &'a str {
        self.raw.str_field("substrate")
    }

    pub fn schema_version(&self) -> f64 {
        self.raw.number_field("schemaVersion")
    }

    pub fn migration_ids(&self) -> Vec<&'a str> {
        self.raw.strs_field("migrationIds")
    }

    pub fn authoring_state(&self) -> &'a Object {
        self.raw.object_field("authoringState")
    }

    pub fn authoring_projection(&self) -> &'a str {
        self.authoring_state().str_field("projection")
    }

    pub fn import_mutation(&self) -> &'a str {
        self.authoring_state().str_field("importMutation")
    }

    pub fn root_container(&self) -> &'a str {
        self.raw.str_field("rootContainer")
    }

    pub fn client_path_naming(&self) -> &'a str {
        self.raw.str_field("clientPathNaming")
    }

    pub fn fields(&self) -> impl Iterator<Item = CollaborationField<'a>> {
        self.raw
            .object_field("fields")
            .iter()
            .map(|(path, raw)| CollaborationField {
                path,
                raw: object(raw, "collaboration field"),
            })
    }

    pub fn field_count(&self) -> usize {
        self.raw.object_field("fields").len()
    }
}

/// One `collaboration.entities.*.fields` entry.
#[derive(Clone, Copy, Debug)]
pub struct CollaborationField<'a> {
    pub path: &'a str,
    pub raw: &'a Object,
}

impl<'a> CollaborationField<'a> {
    pub fn storage(&self) -> &'a Object {
        self.raw.object_field("storage")
    }

    pub fn storage_kind(&self) -> &'a str {
        self.storage().str_field("kind")
    }

    /// A storage setting, present or not.
    pub fn storage_setting(&self, key: &str) -> Option<&'a str> {
        self.storage().opt_str(key)
    }

    pub fn value(&self) -> &'a Object {
        self.raw.object_field("value")
    }

    pub fn codec(&self) -> &'a str {
        self.value().str_field("codec")
    }

    /// The `{ "$ref": ... }` naming the value schema, if there is one.
    pub fn value_schema(&self) -> Option<&'a Object> {
        self.value().get("schema").and_then(Json::as_object)
    }

    pub fn required(&self) -> bool {
        self.raw
            .get("required")
            .and_then(Json::as_bool)
            .expect("the meta-schema requires boolean \"required\"")
    }

    pub fn conflict(&self) -> &'a str {
        self.raw.str_field("conflict")
    }

    /// Whether the field is written by the document's owner rather than its author.
    pub fn is_derived(&self) -> bool {
        matches!(self.storage_kind(), "derivedIdentity" | "derivedRevision")
    }
}
