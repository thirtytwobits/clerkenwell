//! Client spelling: how a TypeScript replica spells a collaborative entity's
//! document.
//!
//! A replica respells every member name a field path declares, and every key
//! of a structured list's entries at any depth. A value a field stores whole as
//! a structured map, a structured document or property text keeps its wire
//! spelling. A replica never materialises a `derivedIdentity` field at the
//! document root, and materialises a `derivedRevision` field only when its
//! caller names a revision. It writes an object only beneath a field it
//! writes, so it leaves out an object no field beneath which is in every
//! document.
//!
//! The generator renders client document types and the fixtures' client
//! documents from this module alone.

use std::collections::{HashMap, HashSet};

use indexmap::{IndexMap, IndexSet};

use crate::definition::{CollaborationEntity, CollaborationField, Definition};
use crate::error::{refuse, Result};
use crate::json::{string_literal, Json, Object};
use crate::names::pascal_identifier;
use crate::schema::{has_type, object_at, read_string_array, schema_ref_type_name};

/// A member name as client documents spell it: an underscore followed by a
/// lower-case letter becomes that letter in upper case.
pub(crate) fn client_name(name: &str) -> String {
    let characters: Vec<char> = name.chars().collect();
    let mut spelled = String::with_capacity(name.len());
    let mut index = 0;
    while index < characters.len() {
        match characters.get(index + 1) {
            Some(next) if characters[index] == '_' && next.is_ascii_lowercase() => {
                spelled.push(next.to_ascii_uppercase());
                index += 2;
            }
            _ => {
                spelled.push(characters[index]);
                index += 1;
            }
        }
    }
    spelled
}

/// The client document type of a `$defs` entry.
pub(crate) fn client_type_name(definition_name: &str) -> String {
    format!("Client{}", pascal_identifier(definition_name))
}

/// How a replica spells the value at a field's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// Each member name respelled, as the plan's paths are.
    Paths,
    /// Every key respelled, at any depth.
    Entries,
    /// In wire spelling.
    Wire,
}

fn held(field: &CollaborationField) -> Held {
    match field.storage_kind() {
        "structuredMap" | "structuredDocument" => Held::Wire,
        "text" if field.codec() == "propertyText" => Held::Wire,
        "structuredList" => Held::Entries,
        _ => Held::Paths,
    }
}

/// Whether a replica may hold the document without the field.
fn materialised_optionally(field: &CollaborationField) -> bool {
    match field.storage_kind() {
        "derivedRevision" => true,
        "derivedIdentity" => !field.path.contains('*'),
        _ => false,
    }
}

fn join(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_owned()
    } else {
        format!("{prefix}.{segment}")
    }
}

/// `wire`, a wire document of `entity`, as a TypeScript replica spells it.
pub(crate) fn client_document(entity: CollaborationEntity, wire: &Json) -> Json {
    let fields: HashMap<&str, CollaborationField> =
        entity.fields().map(|field| (field.path, field)).collect();
    paths_value(&fields, "", wire)
}

fn paths_value(fields: &HashMap<&str, CollaborationField>, path: &str, value: &Json) -> Json {
    if let Some(field) = fields.get(path) {
        return match held(field) {
            Held::Wire => value.clone(),
            Held::Entries => entries_value(value),
            Held::Paths => match value {
                Json::Array(items) if field.storage_kind() == "keyedSequence" => {
                    let item_path = join(path, "*");
                    Json::Array(
                        items
                            .iter()
                            .map(|item| paths_value(fields, &item_path, item))
                            .collect(),
                    )
                }
                other => other.clone(),
            },
        };
    }
    match value {
        Json::Object(members) => Json::Object(
            members
                .iter()
                .map(|(name, member)| {
                    (
                        client_name(name),
                        paths_value(fields, &join(path, name), member),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn entries_value(value: &Json) -> Json {
    match value {
        Json::Object(members) => Json::Object(
            members
                .iter()
                .map(|(name, member)| (client_name(name), entries_value(member)))
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(entries_value).collect()),
        other => other.clone(),
    }
}

/// A schema node within a `$defs` entry: the entry's name and the dotted path
/// to the node, `*` standing for an array's items.
type Position = (String, String);

/// Where each `$defs` entry the collaborative documents hold appears in client
/// spelling, and what of it they hold otherwise.
#[derive(Debug, Default)]
pub(crate) struct ClientSpelling {
    /// The entries with a client document type, in `$defs` order.
    types: IndexSet<String>,
    /// Of `types`, those whose client spelling is their wire spelling.
    unchanged: HashSet<String>,
    /// Members held in wire spelling.
    wire: HashSet<Position>,
    /// Members a replica may leave out although the schema requires them.
    optional: HashSet<Position>,
}

impl ClientSpelling {
    /// The client spelling of every collaborative entity's document, or the
    /// refusal of a definition whose client spelling is ambiguous.
    pub(crate) fn of(definition: &Definition) -> Result<Self> {
        let mut walk = Walk {
            definition,
            fields: HashMap::new(),
            context: String::new(),
            reached: IndexSet::new(),
            in_entries: HashSet::new(),
            respelled: IndexMap::new(),
            wire: IndexMap::new(),
            optional: HashSet::new(),
            checked: HashSet::new(),
        };
        for entity in definition.collaboration().entities() {
            let root = definition
                .entity(entity.name)
                .expect("validation requires the collaborative entity")
                .schema();
            walk.fields = entity.fields().map(|field| (field.path, field)).collect();
            walk.context = format!("collaboration.entities.{}", entity.name);
            let name = local_reference(root).expect("validation requires a $defs entry");
            walk.reached.insert(name.to_owned());
            let schema = definition
                .def(name)
                .expect("validation resolves every reference");
            walk.paths_node(name, "", schema, "")?;
        }
        walk.refuse_ambiguity()?;

        let mut spelling = ClientSpelling {
            wire: walk.wire.into_keys().collect(),
            optional: walk.optional,
            ..ClientSpelling::default()
        };
        for (name, schema) in definition.def_entries() {
            if walk.reached.contains(name) && has_members(definition, schema, &mut HashSet::new()) {
                spelling.types.insert(name.to_owned());
            }
        }
        // An entry's spelling differs from its wire spelling when one of its
        // own members does, or an entry it holds in client spelling does.
        let mut differs: HashSet<String> = HashSet::new();
        loop {
            let found: Vec<String> = spelling
                .types
                .iter()
                .filter(|name| !differs.contains(*name))
                .filter(|name| {
                    let schema = definition.def(name).expect("a $defs entry");
                    spelling.differs(name, "", schema, &differs)
                })
                .cloned()
                .collect();
            if found.is_empty() {
                break;
            }
            differs.extend(found);
        }
        spelling.unchanged = spelling
            .types
            .iter()
            .filter(|name| !differs.contains(*name))
            .cloned()
            .collect();
        Ok(spelling)
    }

    /// The entries with a client document type, in `$defs` order.
    pub(crate) fn types(&self) -> impl Iterator<Item = &str> {
        self.types.iter().map(String::as_str)
    }

    /// Whether the client spelling of `definition_name` is its wire spelling.
    pub(crate) fn is_unchanged(&self, definition_name: &str) -> bool {
        self.unchanged.contains(definition_name)
    }

    /// The type a client document names a local `$defs` reference by.
    pub(crate) fn reference(&self, reference: &Object) -> Result<String> {
        let wire = schema_ref_type_name(reference)?;
        Ok(match local_reference(reference) {
            Some(name) if self.types.contains(name) => client_type_name(name),
            _ => wire,
        })
    }

    /// Whether the member at `path` in `definition_name` is held in wire spelling.
    pub(crate) fn is_wire(&self, definition_name: &str, path: &str) -> bool {
        self.wire
            .contains(&(definition_name.to_owned(), path.to_owned()))
    }

    /// Whether a replica may leave out the member at `path` in `definition_name`.
    pub(crate) fn is_optional(&self, definition_name: &str, path: &str) -> bool {
        self.optional
            .contains(&(definition_name.to_owned(), path.to_owned()))
    }

    /// Whether the node at `path` in the entry `name` is spelled otherwise in
    /// client documents than on the wire, given the entries in `differs` are.
    fn differs(&self, name: &str, path: &str, schema: &Object, differs: &HashSet<String>) -> bool {
        if self.is_wire(name, path) {
            return false;
        }
        if let Some(reference) = local_reference(schema) {
            return differs.contains(reference);
        }
        if let Some(variants) = object_at(schema, "oneOf") {
            let discriminator = schema
                .get("discriminator")
                .and_then(Json::as_str)
                .expect("validation requires a discriminator");
            return client_name(discriminator) != discriminator
                || variants.iter().any(|(tag, variant)| {
                    let variant = variant.as_object().expect("an object variant");
                    self.differs(name, &join(path, tag), variant, differs)
                });
        }
        if let Some(properties) = object_at(schema, "properties") {
            let required = read_string_array(schema.get("required"), "schema.required")
                .expect("validation requires a string array");
            return properties.iter().any(|(member, member_schema)| {
                let member_path = join(path, member);
                let member_schema = member_schema.as_object().expect("an object schema");
                client_name(member) != member
                    || (required.contains(&member) && self.is_optional(name, &member_path))
                    || self.differs(name, &member_path, member_schema, differs)
            });
        }
        if let Some(additional) = object_at(schema, "additionalProperties") {
            return self.differs(name, &join(path, "*"), additional, differs);
        }
        if let Some(items) = object_at(schema, "items") {
            return self.differs(name, &join(path, "*"), items, differs);
        }
        false
    }
}

/// The `$defs` entry a local reference names.
fn local_reference(schema: &Object) -> Option<&str> {
    schema
        .get("$ref")
        .and_then(Json::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
}

/// Whether `schema` declares members a client document could respell: it is,
/// or holds through arrays, an object.
fn has_members<'d>(
    definition: &'d Definition,
    schema: &'d Object,
    visited: &mut HashSet<&'d str>,
) -> bool {
    if let Some(reference) = local_reference(schema) {
        return visited.insert(reference)
            && definition
                .def(reference)
                .is_some_and(|schema| has_members(definition, schema, visited));
    }
    if has_type(schema, "object") {
        return true;
    }
    object_at(schema, "items").is_some_and(|items| has_members(definition, items, visited))
}

/// Whether any member name `schema` declares, through references, is respelled
/// in client spelling.
fn respellable<'d>(
    definition: &'d Definition,
    schema: &'d Object,
    visited: &mut HashSet<&'d str>,
) -> bool {
    if let Some(reference) = local_reference(schema) {
        return visited.insert(reference)
            && definition
                .def(reference)
                .is_some_and(|schema| respellable(definition, schema, visited));
    }
    if let Some(variants) = object_at(schema, "oneOf") {
        let discriminator = schema
            .get("discriminator")
            .and_then(Json::as_str)
            .unwrap_or_default();
        return client_name(discriminator) != discriminator
            || variants.iter().any(|(_, variant)| {
                variant
                    .as_object()
                    .is_some_and(|variant| respellable(definition, variant, visited))
            });
    }
    if let Some(properties) = object_at(schema, "properties") {
        if properties.iter().any(|(member, member_schema)| {
            client_name(member) != member
                || member_schema
                    .as_object()
                    .is_some_and(|member_schema| respellable(definition, member_schema, visited))
        }) {
            return true;
        }
    }
    ["additionalProperties", "items"].iter().any(|key| {
        object_at(schema, key).is_some_and(|child| respellable(definition, child, visited))
    })
}

/// The walk of every collaborative document's schema beside its plan.
struct Walk<'d> {
    definition: &'d Definition,
    /// The fields of the entity being walked, by path.
    fields: HashMap<&'d str, CollaborationField<'d>>,
    /// The entity being walked, for refusals.
    context: String,
    /// Every entry reached in client spelling.
    reached: IndexSet<String>,
    /// Every entry reached inside a structured list's entries.
    in_entries: HashSet<String>,
    /// Every position held in client spelling, with the field that reached it
    /// first.
    respelled: IndexMap<Position, String>,
    /// Every position held in wire spelling, with the field that holds it first.
    wire: IndexMap<Position, String>,
    optional: HashSet<Position>,
    /// The objects whose member spellings were checked.
    checked: HashSet<Position>,
}

impl<'d> Walk<'d> {
    /// `schema`, at `path` within the entry `name`, with every reference
    /// followed: the entry holding the node, the node's path within it, and
    /// the node.
    fn follow(
        &mut self,
        name: &str,
        path: &str,
        mut schema: &'d Object,
    ) -> (String, String, &'d Object) {
        let mut name = name.to_owned();
        let mut path = path.to_owned();
        while let Some(reference) = local_reference(schema) {
            schema = self
                .definition
                .def(reference)
                .expect("validation resolves every reference");
            self.reached.insert(reference.to_owned());
            name = reference.to_owned();
            path = String::new();
        }
        (name, path, schema)
    }

    /// The node at plan path `plan_path`, at `path` within the entry `name`.
    fn paths_node(
        &mut self,
        name: &str,
        path: &str,
        schema: &'d Object,
        plan_path: &str,
    ) -> Result<()> {
        let position = (name.to_owned(), path.to_owned());
        let context = format!("{}.fields.{plan_path}", self.context);
        if let Some(field) = self.fields.get(plan_path).copied() {
            if materialised_optionally(&field) {
                self.optional.insert(position.clone());
            }
            match held(&field) {
                Held::Wire => {
                    self.wire.entry(position).or_insert(context);
                    return Ok(());
                }
                Held::Entries => return self.entries_node(name, path, schema, &context),
                Held::Paths => {
                    self.respelled.entry(position).or_insert(context);
                    if field.storage_kind() != "keyedSequence" {
                        return Ok(());
                    }
                    let (name, path, schema) = self.follow(name, path, schema);
                    return match object_at(schema, "items") {
                        Some(items) => {
                            self.paths_node(&name, &join(&path, "*"), items, &join(plan_path, "*"))
                        }
                        None => Ok(()),
                    };
                }
            }
        }
        // A replica writes an object only beneath a field it writes, so an
        // object member no field beneath it is in every document of may be
        // left out.
        if !plan_path.is_empty() && !plan_path.ends_with('*') && !self.always_beneath(plan_path) {
            self.optional.insert(position.clone());
        }
        self.respelled.entry(position).or_insert(context);
        let (name, path, schema) = self.follow(name, path, schema);
        if let Some(properties) = object_at(schema, "properties") {
            self.check_members(&name, &path, properties.keys())?;
            for (member, member_schema) in properties.iter() {
                let member_schema = member_schema.as_object().expect("an object schema");
                self.paths_node(
                    &name,
                    &join(&path, member),
                    member_schema,
                    &join(plan_path, member),
                )?;
            }
        }
        Ok(())
    }

    /// Whether a field beneath `plan_path` is in every document a replica
    /// materialises.
    fn always_beneath(&self, plan_path: &str) -> bool {
        let prefix = format!("{plan_path}.");
        self.fields.values().any(|field| {
            field.path.starts_with(&prefix) && field.required() && !materialised_optionally(field)
        })
    }

    /// A node inside a structured list's entries, at `path` within the entry
    /// `name`, reached through the field `context` names.
    fn entries_node(
        &mut self,
        name: &str,
        path: &str,
        schema: &'d Object,
        context: &str,
    ) -> Result<()> {
        self.respelled
            .entry((name.to_owned(), path.to_owned()))
            .or_insert_with(|| context.to_owned());
        if let Some(reference) = local_reference(schema) {
            self.reached.insert(reference.to_owned());
            if !self.in_entries.insert(reference.to_owned()) {
                return Ok(());
            }
            let schema = self
                .definition
                .def(reference)
                .expect("validation resolves every reference");
            return self.entries_node(reference, "", schema, context);
        }
        if let Some(variants) = object_at(schema, "oneOf") {
            let discriminator = schema
                .get("discriminator")
                .and_then(Json::as_str)
                .expect("validation requires a discriminator");
            for (tag, variant) in variants.iter() {
                let variant = variant.as_object().expect("an object variant");
                let variant_path = join(path, tag);
                let members = object_at(variant, "properties")
                    .into_iter()
                    .flat_map(Object::keys)
                    .chain([discriminator]);
                self.check_members(name, &variant_path, members)?;
                self.entries_node(name, &variant_path, variant, context)?;
            }
            return Ok(());
        }
        if let Some(properties) = object_at(schema, "properties") {
            self.check_members(name, path, properties.keys())?;
            for (member, member_schema) in properties.iter() {
                let member_schema = member_schema.as_object().expect("an object schema");
                self.entries_node(name, &join(path, member), member_schema, context)?;
            }
        }
        for key in ["additionalProperties", "items"] {
            if let Some(child) = object_at(schema, key) {
                self.entries_node(name, &join(path, "*"), child, context)?;
            }
        }
        Ok(())
    }

    /// Refuses an object two of whose members client documents spell alike.
    fn check_members<'m>(
        &mut self,
        name: &str,
        path: &str,
        members: impl Iterator<Item = &'m str>,
    ) -> Result<()> {
        if !self.checked.insert((name.to_owned(), path.to_owned())) {
            return Ok(());
        }
        let mut spelled: HashMap<String, &str> = HashMap::new();
        for member in members {
            if let Some(other) = spelled.insert(client_name(member), member) {
                let at = if path.is_empty() {
                    format!("$defs.{name}")
                } else {
                    format!("$defs.{name}.{path}")
                };
                return refuse(format!(
                    "{at} declares {} and {}, which client documents both spell {}.",
                    string_literal(other),
                    string_literal(member),
                    string_literal(&client_name(member))
                ));
            }
        }
        Ok(())
    }

    /// Refuses a member held in wire spelling in one document and in client
    /// spelling in another, when the two spellings differ.
    fn refuse_ambiguity(&self) -> Result<()> {
        for ((name, path), holder) in &self.wire {
            let Some(respelled_by) = self.respelled.get(&(name.clone(), path.clone())) else {
                continue;
            };
            let schema = self.schema_at(name, path);
            if respellable(self.definition, schema, &mut HashSet::new()) {
                return refuse(format!(
                    "{holder} keeps $defs.{name}.{path} in wire spelling and {respelled_by} respells it for client documents; declare a $defs entry for each."
                ));
            }
        }
        Ok(())
    }

    /// The schema node at `path` within the entry `name`.
    fn schema_at(&self, name: &str, path: &str) -> &'d Object {
        let mut schema = self.definition.def(name).expect("a $defs entry");
        for segment in path.split('.').filter(|segment| !segment.is_empty()) {
            let child = if segment == "*" {
                object_at(schema, "items")
            } else {
                object_at(schema, "properties")
                    .and_then(|properties| object_at(properties, segment))
            };
            schema = child.expect("a position resolves in its entry");
        }
        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_underscore_before_a_lower_case_letter_becomes_its_upper_case() {
        for (wire, client) in [
            ("world_key", "worldKey"),
            ("a_b_c", "aBC"),
            ("_leading", "Leading"),
        ] {
            assert_eq!(client_name(wire), client);
        }
    }

    #[test]
    fn every_other_character_keeps_its_spelling() {
        for name in [
            "already",
            "trailing_",
            "a_1",
            "a__",
            "a_B",
            "$mime",
            "é_é",
            "kebab-case",
        ] {
            assert_eq!(client_name(name), name);
        }
    }

    #[test]
    fn respelling_is_idempotent() {
        for name in ["world_key", "a__b", "x_y_z", "a_1_b", "$foo_bar"] {
            let once = client_name(name);
            assert_eq!(client_name(&once), once, "{name}");
        }
    }
}
