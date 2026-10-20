//! The other members of an open object: an object schema that declares
//! `properties` beside a typed `additionalProperties`.
//!
//! A generated open object is a struct of its declared members with an
//! [`OtherMembers`] field flattened beside them. An [`OtherMembers`] never
//! holds a member its owner declares, so the object serialises with each
//! member name once.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The members an open object declares, by their wire names, and the
/// validation keywords and defaults its other members' schema carries beyond
/// their Rust type's.
pub trait DeclaredMembers {
    const DECLARED_MEMBERS: &'static [&'static str];
    const OTHER_MEMBER_CONSTRAINTS: &'static [SchemaConstraint] = &[];
}

/// A validation keyword or `default` of an open object's other members'
/// schema, at `path` within it: the `items` and `additionalProperties`
/// keywords leading to the schema it belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SchemaConstraint {
    pub path: &'static [&'static str],
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

/// An open object's members other than those `Owner` declares, in key order.
///
/// Reads go through the map it dereferences to. Every write refuses a member
/// `Owner` declares.
pub struct OtherMembers<Owner, V> {
    members: BTreeMap<String, V>,
    owner: PhantomData<fn() -> Owner>,
}

/// A member name an open object declares, given as one of its other members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredMemberError {
    name: String,
}

impl DeclaredMemberError {
    /// The declared member's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for DeclaredMemberError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} is a declared member, not one of the other members",
            self.name
        )
    }
}

impl std::error::Error for DeclaredMemberError {}

fn refuse_declared<Owner: DeclaredMembers>(name: &str) -> Result<(), DeclaredMemberError> {
    if Owner::DECLARED_MEMBERS.contains(&name) {
        return Err(DeclaredMemberError {
            name: name.to_owned(),
        });
    }
    Ok(())
}

impl<Owner, V> OtherMembers<Owner, V> {
    /// No other members.
    pub fn new() -> Self {
        Self {
            members: BTreeMap::new(),
            owner: PhantomData,
        }
    }

    /// The value of the other member `name`, to change in place.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut V> {
        self.members.get_mut(name)
    }

    /// Removes the other member `name`, returning its value.
    pub fn remove(&mut self, name: &str) -> Option<V> {
        self.members.remove(name)
    }

    /// The other members as a map.
    pub fn into_map(self) -> BTreeMap<String, V> {
        self.members
    }
}

impl<Owner: DeclaredMembers, V> OtherMembers<Owner, V> {
    /// Sets the other member `name`, returning the value it replaces, or
    /// refuses a name `Owner` declares.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        value: V,
    ) -> Result<Option<V>, DeclaredMemberError> {
        let name = name.into();
        refuse_declared::<Owner>(&name)?;
        Ok(self.members.insert(name, value))
    }
}

impl<Owner: DeclaredMembers, V> TryFrom<BTreeMap<String, V>> for OtherMembers<Owner, V> {
    type Error = DeclaredMemberError;

    /// Refuses a map holding a member `Owner` declares.
    fn try_from(members: BTreeMap<String, V>) -> Result<Self, Self::Error> {
        for name in members.keys() {
            refuse_declared::<Owner>(name)?;
        }
        Ok(Self {
            members,
            owner: PhantomData,
        })
    }
}

impl<Owner, V> Deref for OtherMembers<Owner, V> {
    type Target = BTreeMap<String, V>;

    fn deref(&self) -> &Self::Target {
        &self.members
    }
}

impl<'a, Owner, V> IntoIterator for &'a OtherMembers<Owner, V> {
    type Item = (&'a String, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.members.iter()
    }
}

impl<Owner, V> IntoIterator for OtherMembers<Owner, V> {
    type Item = (String, V);
    type IntoIter = std::collections::btree_map::IntoIter<String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.members.into_iter()
    }
}

impl<Owner, V> Default for OtherMembers<Owner, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Owner, V: Clone> Clone for OtherMembers<Owner, V> {
    fn clone(&self) -> Self {
        Self {
            members: self.members.clone(),
            owner: PhantomData,
        }
    }
}

impl<Owner, V: fmt::Debug> fmt::Debug for OtherMembers<Owner, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.members.fmt(formatter)
    }
}

impl<Owner, V: PartialEq> PartialEq for OtherMembers<Owner, V> {
    fn eq(&self, other: &Self) -> bool {
        self.members == other.members
    }
}

impl<Owner, V: Eq> Eq for OtherMembers<Owner, V> {}

impl<Owner, V: Serialize> Serialize for OtherMembers<Owner, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.members.serialize(serializer)
    }
}

impl<'de, Owner: DeclaredMembers, V: Deserialize<'de>> Deserialize<'de> for OtherMembers<Owner, V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let members = BTreeMap::<String, V>::deserialize(deserializer)?;
        Self::try_from(members).map_err(D::Error::custom)
    }
}

/// The schema of a map of `V`, with `Owner`'s other-member constraints.
impl<Owner: DeclaredMembers, V: JsonSchema> JsonSchema for OtherMembers<Owner, V> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        BTreeMap::<String, V>::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        BTreeMap::<String, V>::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = BTreeMap::<String, V>::json_schema(generator);
        for constraint in Owner::OTHER_MEMBER_CONSTRAINTS {
            let pointer: String = std::iter::once("additionalProperties")
                .chain(constraint.path.iter().copied())
                .map(|keyword| format!("/{keyword}"))
                .collect();
            let target = schema
                .pointer_mut(&pointer)
                .and_then(serde_json::Value::as_object_mut)
                .expect("a constraint's path names an object schema within its type's");
            target.insert(constraint.keyword.to_owned(), constraint.value.into());
        }
        schema
    }
}
