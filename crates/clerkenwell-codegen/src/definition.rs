//! A validated projection definition and typed views over it.
//!
//! The views borrow the parsed document rather than copying it into structs:
//! several artefacts reproduce a section exactly as it was written, key order
//! included, so the document stays the one representation.

use std::path::Path;

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
    pub fn load(path: &Path) -> Result<Definition> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        let document = Json::parse(&text).map_err(|error| Error::Parse {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
        Definition::from_json(document)
    }

    /// Validates `document` against the meta-schema and the semantic rules.
    pub(crate) fn from_json(document: Json) -> Result<Definition> {
        validate::check_meta_schema(&document)?;
        let Json::Object(document) = document else {
            return Err(Error::Definition(
                "Projection definition must be a JSON object.".to_owned(),
            ));
        };
        let definition = Definition {
            document: crate::expand::expand(document)?,
        };
        validate::check_semantics(&definition)?;
        Ok(definition)
    }

    /// The document as parsed.
    pub(crate) fn document(&self) -> &Object {
        &self.document
    }

    pub(crate) fn version(&self) -> f64 {
        self.document.number_field("version")
    }

    pub(crate) fn namespace(&self) -> &str {
        self.document.str_field("namespace")
    }

    fn section(&self, key: &str) -> &Object {
        self.document.object_field(key)
    }

    pub(crate) fn entities(&self) -> impl Iterator<Item = Entity<'_>> {
        self.section("entities")
            .iter()
            .map(|(name, raw)| Entity::new(name, raw))
    }

    pub(crate) fn entity(&self, name: &str) -> Option<Entity<'_>> {
        self.entities().find(|entity| entity.name == name)
    }

    pub(crate) fn entity_names(&self) -> Vec<&str> {
        self.section("entities").keys().collect()
    }

    pub(crate) fn projections(&self) -> impl Iterator<Item = Projection<'_>> {
        self.section("projections")
            .iter()
            .map(|(name, raw)| Projection {
                name,
                raw: object(raw, "projection"),
            })
    }

    pub(crate) fn projection(&self, name: &str) -> Option<Projection<'_>> {
        self.projections()
            .find(|projection| projection.name == name)
    }

    pub(crate) fn projection_count(&self) -> usize {
        self.section("projections").len()
    }

    pub(crate) fn mutations(&self) -> impl Iterator<Item = Mutation<'_>> {
        self.section("mutations")
            .iter()
            .map(|(name, raw)| Mutation {
                name,
                raw: object(raw, "mutation"),
            })
    }

    pub(crate) fn mutation(&self, name: &str) -> Option<Mutation<'_>> {
        self.mutations().find(|mutation| mutation.name == name)
    }

    pub(crate) fn collaboration(&self) -> Collaboration<'_> {
        Collaboration {
            raw: self.section("collaboration"),
        }
    }

    /// The `$defs` section.
    pub(crate) fn defs(&self) -> &Object {
        self.section("$defs")
    }

    pub(crate) fn def(&self, name: &str) -> Option<&Object> {
        self.defs().get(name).and_then(Json::as_object)
    }

    pub(crate) fn def_entries(&self) -> impl Iterator<Item = (&str, &Object)> {
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

    pub fn planning_mutations(&self) -> Vec<&'a str> {
        self.authoring().strs_field("planningMutations")
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

    pub fn touches(&self) -> Vec<&'a str> {
        self.raw.strs_field("touches")
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
    pub fn schema_version(&self) -> f64 {
        self.raw.number_field("schemaVersion")
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

    /// The resource id of the one document every subscription to the
    /// authoring state follows, when the entity declares one.
    pub fn authoring_document(&self) -> Option<&'a str> {
        self.authoring_state().opt_str("document")
    }

    /// The authoring state's parameters that choose a document's store.
    pub fn authoring_store_params(&self) -> Vec<&'a str> {
        self.authoring_state()
            .opt_strs_field("storeParams")
            .unwrap_or_default()
    }

    pub fn root_container(&self) -> &'a str {
        self.raw.str_field("rootContainer")
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

    /// Whether every segment of the path is required, so the field is in
    /// every document.
    pub fn required(&self) -> bool {
        self.raw
            .get("required")
            .and_then(Json::as_bool)
            .expect("the meta-schema requires boolean \"required\"")
    }

    /// Whether the object holding the field requires it, so the field is
    /// present whenever that object is. A required field always is; the
    /// definition says so of an optional field its parent requires.
    pub fn required_in_parent(&self) -> bool {
        self.raw
            .get("requiredInParent")
            .and_then(Json::as_bool)
            .unwrap_or_else(|| self.required())
    }

    pub fn conflict(&self) -> &'a str {
        self.raw.str_field("conflict")
    }

    /// Each kind of writer judged by another policy than the field's own,
    /// and that policy, in declaration order.
    pub fn writers(&self) -> Vec<(&'a str, &'a str)> {
        self.raw
            .get("writers")
            .and_then(Json::as_object)
            .map(|writers| {
                writers
                    .iter()
                    .map(|(kind, conflict)| {
                        (
                            kind,
                            conflict
                                .as_str()
                                .expect("the meta-schema requires a string policy"),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the field is written by the document's owner rather than its author.
    pub fn is_derived(&self) -> bool {
        matches!(self.storage_kind(), "derivedIdentity" | "derivedRevision")
    }
}
