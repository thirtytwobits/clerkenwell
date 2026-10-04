//! A crate whose public paths the recorder reads. It is never compiled.

mod hidden;
pub mod open;

pub use external_crate::Borrowed;
pub use whole_crate;
pub use hidden::Reexported;

use serde_json::Value;

pub struct Visible {
    pub field: Value,
    kept: u8,
}

impl Visible {
    pub fn method(&self) -> Option<external_crate::Named> {
        None
    }

    fn private_method(&self) {}
}

impl std::fmt::Display for Visible {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("visible")
    }
}

pub(crate) fn crate_only() {}

#[cfg(feature = "extra")]
pub fn behind_a_feature() {}

#[cfg(feature = "extra")]
pub mod gated {
    mod inner {
        pub struct GatedType;

        impl GatedType {
            pub fn gated_method(&self) {}
        }
    }

    pub use inner::GatedType;
}

#[cfg(test)]
pub fn only_in_tests() {}
