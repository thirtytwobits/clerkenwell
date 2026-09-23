//! The JSON value model the generator reads and writes.
//!
//! The artefacts are defined by ECMAScript semantics, and this model keeps them:
//! numbers are IEEE doubles, an object iterates its array-index keys first in
//! ascending numeric order and every other key in insertion order, and
//! [`Json::stringify`] and [`Json::stringify_pretty`] are `JSON.stringify`.

use std::fmt;

use indexmap::IndexMap;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Object),
}

/// A JSON object in ECMAScript property order. Equality ignores order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Object {
    entries: IndexMap<String, Json>,
}

/// The value of `key` as an ECMAScript array index, if it is one.
fn array_index(key: &str) -> Option<u32> {
    let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty());
    if !canonical || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

impl Object {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        self.entries.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        self.entries.get_mut(key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Assigns `key`: an existing key keeps its position, a new one takes the
    /// position ECMAScript gives it.
    pub fn insert(&mut self, key: impl Into<String>, value: Json) {
        let key = key.into();
        if let Some(slot) = self.entries.get_mut(&key) {
            *slot = value;
            return;
        }
        match array_index(&key) {
            Some(index) => {
                let position = self
                    .entries
                    .keys()
                    .take_while(|existing| array_index(existing).is_some_and(|other| other < index))
                    .count();
                self.entries.shift_insert(position, key, value);
            }
            None => {
                self.entries.insert(key, value);
            }
        }
    }

    /// Assigns `key` when there is a value, as a spread of `{ key: undefined }` would
    /// leave nothing behind once stringified.
    pub fn insert_some(&mut self, key: impl Into<String>, value: Option<Json>) {
        if let Some(value) = value {
            self.insert(key, value);
        }
    }

    /// Removes `key`, keeping the order of the rest.
    pub fn remove(&mut self, key: &str) -> Option<Json> {
        self.entries.shift_remove(key)
    }

    /// `{ ...self, ...other }`.
    pub fn spread(&mut self, other: &Object) {
        for (key, value) in other.iter() {
            self.insert(key, value.clone());
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Json)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn values(&self) -> impl Iterator<Item = &Json> {
        self.entries.values()
    }
}

impl FromIterator<(String, Json)> for Object {
    fn from_iter<T: IntoIterator<Item = (String, Json)>>(iter: T) -> Self {
        let mut object = Object::new();
        for (key, value) in iter {
            object.insert(key, value);
        }
        object
    }
}

impl<'a> FromIterator<(&'a str, Json)> for Object {
    fn from_iter<T: IntoIterator<Item = (&'a str, Json)>>(iter: T) -> Self {
        iter.into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }
}

impl IntoIterator for Object {
    type Item = (String, Json);
    type IntoIter = indexmap::map::IntoIter<String, Json>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl Json {
    /// `JSON.parse`.
    pub fn parse(text: &str) -> Result<Json, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Json::Object(object) => Some(object),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut Object> {
        match self {
            Json::Object(object) => Some(object),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// The property `key` of an object; `None` for anything else.
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.as_object().and_then(|object| object.get(key))
    }

    /// `JSON.stringify(value)`.
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        write_value(&mut out, self, None, "");
        out
    }

    /// `JSON.stringify(value, null, 2)`.
    pub fn stringify_pretty(&self) -> String {
        let mut out = String::new();
        write_value(&mut out, self, Some("  "), "");
        out
    }

    /// The same value as a `serde_json` value, integral numbers as integers.
    pub fn to_serde(&self) -> serde_json::Value {
        match self {
            Json::Null => serde_json::Value::Null,
            Json::Bool(value) => serde_json::Value::Bool(*value),
            Json::Number(value) => {
                if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
                    serde_json::Value::from(*value as i64)
                } else {
                    serde_json::Number::from_f64(*value)
                        .map_or(serde_json::Value::Null, serde_json::Value::Number)
                }
            }
            Json::String(value) => serde_json::Value::String(value.clone()),
            Json::Array(items) => {
                serde_json::Value::Array(items.iter().map(Json::to_serde).collect())
            }
            Json::Object(object) => serde_json::Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.to_owned(), value.to_serde()))
                    .collect(),
            ),
        }
    }
}

impl From<&str> for Json {
    fn from(value: &str) -> Self {
        Json::String(value.to_owned())
    }
}

impl From<String> for Json {
    fn from(value: String) -> Self {
        Json::String(value)
    }
}

impl From<f64> for Json {
    fn from(value: f64) -> Self {
        Json::Number(value)
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Json::Bool(value)
    }
}

impl From<Object> for Json {
    fn from(value: Object) -> Self {
        Json::Object(value)
    }
}

impl From<Vec<Json>> for Json {
    fn from(value: Vec<Json>) -> Self {
        Json::Array(value)
    }
}

impl From<Vec<&str>> for Json {
    fn from(value: Vec<&str>) -> Self {
        Json::Array(value.into_iter().map(Json::from).collect())
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Json, E> {
        Ok(Json::Number(value as f64))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Json, E> {
        Ok(Json::Number(value as f64))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Json, E> {
        Ok(Json::Number(value))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Json, E> {
        Ok(Json::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Json, E> {
        Ok(Json::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut object = Object::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            object.insert(key, value);
        }
        Ok(Json::Object(object))
    }
}

/// `JSON.stringify(value)` for a string: the quoted, escaped literal.
pub fn string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    write_string(&mut out, value);
    out
}

/// `Number.prototype.toString()`, which is also how a template literal and
/// `JSON.stringify` spell a finite number.
pub fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    // `{:e}` renders the shortest digit string that round-trips, as
    // Number::toString requires; only the placement of the point differs.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("exponential formatting always carries an exponent");
    let exponent: i32 = exponent
        .parse()
        .expect("exponential formatting writes an integer exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let exponent_sign = if e >= 0 { '+' } else { '-' };
        if k == 1 {
            format!("{digits}e{exponent_sign}{}", e.abs())
        } else {
            format!(
                "{}.{}e{exponent_sign}{}",
                &digits[..1],
                &digits[1..],
                e.abs()
            )
        }
    };
    format!("{sign}{body}")
}

fn write_string(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, value: &Json, gap: Option<&str>, indent: &str) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Number(number) if number.is_finite() => out.push_str(&number_to_string(*number)),
        Json::Number(_) => out.push_str("null"),
        Json::String(text) => write_string(out, text),
        Json::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            write_container(
                out,
                '[',
                ']',
                gap,
                indent,
                items.iter(),
                |out, item, inner| {
                    write_value(out, item, gap, inner);
                },
            );
        }
        Json::Object(object) => {
            if object.is_empty() {
                out.push_str("{}");
                return;
            }
            write_container(
                out,
                '{',
                '}',
                gap,
                indent,
                object.iter(),
                |out, (key, item), inner| {
                    write_string(out, key);
                    out.push(':');
                    if gap.is_some() {
                        out.push(' ');
                    }
                    write_value(out, item, gap, inner);
                },
            );
        }
    }
}

fn write_container<T>(
    out: &mut String,
    open: char,
    close: char,
    gap: Option<&str>,
    indent: &str,
    members: impl Iterator<Item = T>,
    mut write_member: impl FnMut(&mut String, T, &str),
) {
    out.push(open);
    match gap {
        Some(gap) => {
            let inner = format!("{indent}{gap}");
            for (index, member) in members.enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&inner);
                write_member(out, member, &inner);
            }
            out.push('\n');
            out.push_str(indent);
        }
        None => {
            for (index, member) in members.enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_member(out, member, indent);
            }
        }
    }
    out.push(close);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_index_keys_iterate_first_in_numeric_order() {
        let object = Json::parse(r#"{"b": 1, "10": 2, "a": 3, "2": 4, "02": 5}"#).unwrap();
        let keys: Vec<&str> = object.as_object().unwrap().keys().collect();
        let indices: Vec<&str> = keys
            .iter()
            .copied()
            .take_while(|key| array_index(key).is_some())
            .collect();
        let mut sorted = indices.clone();
        sorted.sort_by_key(|key| array_index(key));
        assert_eq!(indices, sorted);
        assert!(keys[indices.len()..]
            .iter()
            .all(|key| array_index(key).is_none()));
        assert_eq!(indices.len(), 2);
    }

    #[test]
    fn a_repeated_key_keeps_its_first_position_and_its_last_value() {
        let object = Json::parse(r#"{"a": 1, "b": 2, "a": 3}"#).unwrap();
        let object = object.as_object().unwrap();
        assert_eq!(object.keys().next(), Some("a"));
        assert_eq!(object.get("a"), Some(&Json::Number(3.0)));
    }

    #[test]
    fn stringified_values_parse_back_to_themselves() {
        let source = r#"{"text":"quote \" slash \\ tab \t nul \u0000 é","numbers":[0,-1,1.5,1e21,1e-7,123456789012,0.000001],"nested":{"empty":{},"list":[],"flag":true,"nothing":null}}"#;
        let value = Json::parse(source).unwrap();
        assert_eq!(Json::parse(&value.stringify()).unwrap(), value);
        assert_eq!(Json::parse(&value.stringify_pretty()).unwrap(), value);
    }

    #[test]
    fn numbers_render_in_their_shortest_round_tripping_form() {
        for value in [
            1.0,
            1.5,
            0.1,
            1e21,
            1e-7,
            123e-20,
            2f64.powi(60),
            f64::MIN_POSITIVE,
        ] {
            let rendered = number_to_string(value);
            assert_eq!(rendered.parse::<f64>().unwrap(), value, "{rendered}");
            assert!(!rendered.ends_with(".0"), "{rendered}");
        }
    }
}
