//! Members that are absent, `null` or a value.

use serde::{Deserialize, Deserializer};

/// Deserialises a member that is present, `null` included, as `Some`.
///
/// With `#[serde(default, deserialize_with = "present")]` and
/// `skip_serializing_if = "Option::is_none"`, an `Option<Option<T>>` field is
/// `None` when the member is absent, `Some(None)` when it is `null` and
/// `Some(Some(value))` when it holds a value, and each is written back as it
/// was read.
pub fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
