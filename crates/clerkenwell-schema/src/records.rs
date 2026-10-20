//! The members of a record: an object schema whose members are all values of
//! its typed `additionalProperties`.
//!
//! A generated record `$defs` entry is a [`RecordMembers`] whose
//! `Constraints` names the validation keywords and defaults of its values'
//! schema, so the schema derived for the record carries them wherever it is
//! used.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::constraints::SchemaConstraint;

/// The validation keywords and defaults a record's values' schema carries
/// beyond their Rust type's.
pub trait RecordConstraints {
    const VALUE_CONSTRAINTS: &'static [SchemaConstraint];
}

/// A record's members, in key order.
///
/// It reads and writes as the map it dereferences to.
pub struct RecordMembers<Constraints, V> {
    members: BTreeMap<String, V>,
    constraints: PhantomData<fn() -> Constraints>,
}

impl<Constraints, V> RecordMembers<Constraints, V> {
    /// No members.
    pub fn new() -> Self {
        Self::from(BTreeMap::new())
    }

    /// The members as a map.
    pub fn into_map(self) -> BTreeMap<String, V> {
        self.members
    }
}

impl<Constraints, V> From<BTreeMap<String, V>> for RecordMembers<Constraints, V> {
    fn from(members: BTreeMap<String, V>) -> Self {
        Self {
            members,
            constraints: PhantomData,
        }
    }
}

impl<Constraints, V> FromIterator<(String, V)> for RecordMembers<Constraints, V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(members: I) -> Self {
        Self::from(BTreeMap::from_iter(members))
    }
}

impl<Constraints, V> Deref for RecordMembers<Constraints, V> {
    type Target = BTreeMap<String, V>;

    fn deref(&self) -> &Self::Target {
        &self.members
    }
}

impl<Constraints, V> DerefMut for RecordMembers<Constraints, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.members
    }
}

impl<'a, Constraints, V> IntoIterator for &'a RecordMembers<Constraints, V> {
    type Item = (&'a String, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.members.iter()
    }
}

impl<Constraints, V> IntoIterator for RecordMembers<Constraints, V> {
    type Item = (String, V);
    type IntoIter = std::collections::btree_map::IntoIter<String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.members.into_iter()
    }
}

impl<Constraints, V> Default for RecordMembers<Constraints, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Constraints, V: Clone> Clone for RecordMembers<Constraints, V> {
    fn clone(&self) -> Self {
        Self::from(self.members.clone())
    }
}

impl<Constraints, V: fmt::Debug> fmt::Debug for RecordMembers<Constraints, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.members.fmt(formatter)
    }
}

impl<Constraints, V: PartialEq> PartialEq for RecordMembers<Constraints, V> {
    fn eq(&self, other: &Self) -> bool {
        self.members == other.members
    }
}

impl<Constraints, V: Eq> Eq for RecordMembers<Constraints, V> {}

impl<Constraints, V: Serialize> Serialize for RecordMembers<Constraints, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.members.serialize(serializer)
    }
}

impl<'de, Constraints, V: Deserialize<'de>> Deserialize<'de> for RecordMembers<Constraints, V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        BTreeMap::deserialize(deserializer).map(Self::from)
    }
}

/// The schema of a map of `V`, with `Constraints`' value constraints.
impl<Constraints: RecordConstraints, V: JsonSchema> JsonSchema for RecordMembers<Constraints, V> {
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
        for constraint in Constraints::VALUE_CONSTRAINTS {
            constraint.write(&mut schema, "/additionalProperties");
        }
        schema
    }
}
