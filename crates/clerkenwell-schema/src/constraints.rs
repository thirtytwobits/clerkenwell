//! The validation keywords and defaults a generated type's derived schema
//! carries beyond its Rust type's.

use schemars::transform::Transform;
use schemars::Schema;

/// A validation keyword or `default` of a schema, at `path` within it: the
/// JSON Pointer, made of `items` and `additionalProperties`, of the schema it
/// belongs to.
///
/// As a `schemars` transform, it constrains the schema it transforms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SchemaConstraint {
    pub path: &'static str,
    pub keyword: &'static str,
    pub value: SchemaConstraintValue,
}

/// The value of a [`SchemaConstraint`]'s keyword: a scalar, or any other
/// JSON value as its text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SchemaConstraintValue {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    String(&'static str),
    Json(&'static str),
}

impl From<SchemaConstraintValue> for serde_json::Value {
    fn from(value: SchemaConstraintValue) -> Self {
        match value {
            SchemaConstraintValue::Boolean(value) => value.into(),
            SchemaConstraintValue::Integer(value) => value.into(),
            SchemaConstraintValue::Number(value) => value.into(),
            SchemaConstraintValue::String(value) => value.into(),
            SchemaConstraintValue::Json(text) => {
                serde_json::from_str(text).expect("a constraint's JSON text is JSON")
            }
        }
    }
}

impl SchemaConstraint {
    /// `keyword`, with `value`, of the schema at `path`.
    pub const fn new(
        path: &'static str,
        keyword: &'static str,
        value: SchemaConstraintValue,
    ) -> Self {
        Self {
            path,
            keyword,
            value,
        }
    }

    /// Writes the keyword into the schema at `path` within the schema at the
    /// JSON Pointer `base` within `schema`.
    pub(crate) fn write(&self, schema: &mut Schema, base: &str) {
        let target = schema
            .pointer_mut(&format!("{base}{}", self.path))
            .and_then(serde_json::Value::as_object_mut)
            .expect("a constraint's path names an object schema within its type's");
        target.insert(self.keyword.to_owned(), self.value.into());
    }
}

impl Transform for SchemaConstraint {
    fn transform(&mut self, schema: &mut Schema) {
        self.write(schema, "");
    }
}
