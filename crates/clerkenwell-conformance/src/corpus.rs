//! A generated collaboration fixture corpus: one sample document per
//! collaborative entity, in wire and client naming, with the operations and
//! invalid cases the generator derives from the entity's plan.

use std::path::Path;

use serde_json::Value;

use crate::documents::{pointers, remove, set};

/// A loaded collaboration fixture corpus.
#[derive(Debug, Clone)]
pub struct Corpus {
    entities: Vec<EntityFixture>,
}

/// One collaborative entity's fixture.
#[derive(Debug, Clone)]
pub struct EntityFixture {
    pub entity: String,
    pub schema_version: u32,
    pub wire_document: Value,
    pub client_document: Value,
    operations: Value,
    invalid: Vec<Value>,
}

/// A whole-document edit one fixture operation describes.
pub struct Edit {
    pub name: String,
    edit: Box<dyn Fn(&Value) -> Value>,
}

impl Edit {
    fn new(name: String, edit: impl Fn(&Value) -> Value + 'static) -> Self {
        Self {
            name,
            edit: Box::new(edit),
        }
    }

    /// `document` with this edit applied.
    pub fn apply(&self, document: &Value) -> Value {
        (self.edit)(document)
    }
}

impl Corpus {
    /// Reads the corpus at `path`.
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let corpus: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let entities = corpus["entities"]
            .as_array()
            .unwrap_or_else(|| panic!("{} lists its entities", path.display()))
            .iter()
            .map(|fixture| EntityFixture {
                entity: string(&fixture["entity"]),
                schema_version: fixture["schemaVersion"]
                    .as_u64()
                    .and_then(|version| u32::try_from(version).ok())
                    .expect("a fixture schema version"),
                wire_document: fixture["wireDocument"].clone(),
                client_document: fixture["clientDocument"].clone(),
                operations: fixture["operations"].clone(),
                invalid: fixture["invalid"].as_array().cloned().unwrap_or_default(),
            })
            .collect();
        Self { entities }
    }

    pub fn entities(&self) -> &[EntityFixture] {
        &self.entities
    }
}

fn string(value: &Value) -> String {
    value.as_str().expect("a string").to_string()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|entries| entries.iter().map(string).collect())
        .unwrap_or_default()
}

impl EntityFixture {
    /// The schema versions the fixture's invalid cases say a replica refuses.
    pub fn unsupported_schema_versions(&self) -> Vec<u32> {
        self.invalid
            .iter()
            .filter(|case| case["kind"] == "unsupportedSchemaVersion")
            .map(|case| {
                case["schemaVersion"]
                    .as_u64()
                    .and_then(|version| u32::try_from(version).ok())
                    .expect("an unsupported schema version")
            })
            .collect()
    }

    /// Every edit the fixture's operations describe, in the order a replica
    /// applies them one after another: the scalar edit when the entity has an
    /// editable scalar, removing then restoring the optional fields, each
    /// reorder, then each delete. Paths with `*` apply to every item of the
    /// enclosing sequences.
    pub fn edits(&self) -> Vec<Edit> {
        let operations = &self.operations;
        let mut edits = Vec::new();

        let scalar = operations["scalar"].clone();
        if let Some(path) = scalar["path"].as_str().map(str::to_string) {
            edits.push(Edit::new(format!("scalar {path}"), move |document| {
                let mut edited = document.clone();
                for at in pointers(document, &path) {
                    set(&mut edited, &at, scalar["value"].clone());
                }
                edited
            }));
        }

        let absent = strings(&operations["optionalAbsent"]);
        edits.push(Edit::new(
            format!("optionalAbsent {absent:?}"),
            move |document| {
                let mut edited = document.clone();
                for path in &absent {
                    for at in pointers(document, path) {
                        remove(&mut edited, &at);
                    }
                }
                edited
            },
        ));

        let present = strings(&operations["optionalPresent"]);
        let sample = self.wire_document.clone();
        edits.push(Edit::new(
            format!("optionalPresent {present:?}"),
            move |document| {
                let mut edited = document.clone();
                for path in &present {
                    for at in pointers(&sample, path) {
                        let value = sample.pointer(&at).expect("a fixture value").clone();
                        set(&mut edited, &at, value);
                    }
                }
                edited
            },
        ));

        for reorder in operations["reorder"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let path = string(&reorder["path"]);
            let order = reorder["order"]
                .as_array()
                .expect("a reorder order")
                .iter()
                .map(|index| index.as_u64().expect("an index") as usize)
                .collect::<Vec<_>>();
            edits.push(Edit::new(format!("reorder {path}"), move |document| {
                let mut edited = document.clone();
                for at in pointers(document, &path) {
                    let items = document
                        .pointer(&at)
                        .and_then(Value::as_array)
                        .expect("a sequence");
                    let reordered = order.iter().map(|index| items[*index].clone()).collect();
                    set(&mut edited, &at, Value::Array(reordered));
                }
                edited
            }));
        }

        for delete in operations["delete"].as_array().cloned().unwrap_or_default() {
            let path = string(&delete["path"]);
            let index = delete["index"].as_u64().expect("a delete index") as usize;
            edits.push(Edit::new(format!("delete {path}"), move |document| {
                let mut edited = document.clone();
                for at in pointers(document, &path) {
                    edited
                        .pointer_mut(&at)
                        .and_then(Value::as_array_mut)
                        .expect("a sequence")
                        .remove(index);
                }
                edited
            }));
        }
        edits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(scalar: Value) -> EntityFixture {
        EntityFixture {
            entity: "Note".to_string(),
            schema_version: 1,
            wire_document: json!({ "title": "Before" }),
            client_document: json!({ "title": "Before" }),
            operations: json!({
                "scalar": scalar,
                "optionalAbsent": [],
                "optionalPresent": [],
                "reorder": [],
                "delete": []
            }),
            invalid: Vec::new(),
        }
    }

    #[test]
    fn the_scalar_edit_writes_its_value_at_its_path() {
        let edits = fixture(json!({ "path": "title", "value": "After" })).edits();
        let edited = edits
            .iter()
            .fold(json!({ "title": "Before" }), |document, edit| {
                edit.apply(&document)
            });

        assert_eq!(edited["title"], "After");
    }

    #[test]
    fn an_entity_with_no_editable_scalar_has_no_scalar_edit() {
        let edits = fixture(json!({ "value": "After" })).edits();

        assert!(edits.iter().all(|edit| !edit.name.starts_with("scalar")));
    }
}
