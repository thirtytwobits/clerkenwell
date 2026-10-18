//! The order of the modifications behind an application's projections.
//!
//! A host takes a number from its [`ModificationCounter`] for each
//! modification it saves, and reports with each snapshot the number of the
//! last modification the snapshot holds. A subscription is then sent each
//! later modification once, and none its snapshot holds.

use std::sync::atomic::{AtomicU64, Ordering};

/// Numbers the modifications a server program saves, in the order it saves
/// them.
///
/// The host takes a modification's number while holding whatever keeps a
/// snapshot from reading the data it saves, and reads [`Self::last`] for a
/// snapshot while holding the same, so that a snapshot holds every
/// modification numbered up to the number it reports and none after it.
#[derive(Debug, Default)]
pub struct ModificationCounter {
    last: AtomicU64,
}

impl ModificationCounter {
    /// The next modification's number, which no other modification has.
    pub fn take(&self) -> u64 {
        self.last.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The number of the last modification taken; 0 before any.
    pub fn last(&self) -> u64 {
        self.last.load(Ordering::SeqCst)
    }
}

/// A snapshot, and the number of the last modification it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct NumberedSnapshot<S> {
    pub snapshot: S,
    pub through: u64,
}

/// A mutation's or command's result, and the number of the modification its
/// updates describe; `None` when they describe none.
#[derive(Debug, Clone, PartialEq)]
pub struct Modified<R> {
    pub result: R,
    pub modification: Option<u64>,
}
