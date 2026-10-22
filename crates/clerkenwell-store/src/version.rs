//! Document versions: which history a document holds, and how far into it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a document has accepted: its incarnation, and for each peer the
/// counter after its last accepted operation.
///
/// A document gets a new incarnation whenever it gets a new history: when it
/// is created, and when a migration rewrites it. Two versions compare only
/// within one incarnation, so a document removed and created again never
/// matches a version read before the removal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CollaborationVersion {
    pub incarnation: String,
    pub peers: BTreeMap<u64, u64>,
}

impl CollaborationVersion {
    /// Whether this version holds every operation `other` holds. Never true
    /// across incarnations.
    pub fn covers(&self, other: &Self) -> bool {
        self.incarnation == other.incarnation
            && other
                .peers
                .iter()
                .all(|(peer, counter)| self.peers.get(peer).is_some_and(|held| held >= counter))
    }
}

/// What a peer holds of a document, so that an authoring state sends it only
/// the operations it lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollaborationHeld<'a> {
    /// Every operation up to an accepted frontier the peer read.
    Frontier(&'a str),
    /// Every operation a version names.
    Version(&'a CollaborationVersion),
}

/// A new incarnation, for a document given a new history.
pub(crate) fn new_incarnation() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(incarnation: &str, peers: &[(u64, u64)]) -> CollaborationVersion {
        CollaborationVersion {
            incarnation: incarnation.to_string(),
            peers: peers.iter().copied().collect(),
        }
    }

    #[test]
    fn a_version_covers_itself() {
        let held = version("one", &[(1, 3), (2, 5)]);
        assert!(held.covers(&held));
    }

    #[test]
    fn a_version_covers_one_that_holds_less_of_each_peer() {
        let earlier = version("one", &[(1, 3), (2, 5)]);
        let later = version("one", &[(1, 4), (2, 5), (3, 1)]);
        assert!(later.covers(&earlier));
        assert!(!earlier.covers(&later));
    }

    #[test]
    fn concurrent_versions_cover_neither_way() {
        let left = version("one", &[(1, 4), (2, 5)]);
        let right = version("one", &[(1, 3), (2, 6)]);
        assert!(!left.covers(&right));
        assert!(!right.covers(&left));
    }

    #[test]
    fn every_version_of_an_incarnation_covers_an_empty_one() {
        assert!(version("one", &[(1, 3)]).covers(&version("one", &[])));
    }

    #[test]
    fn versions_of_different_incarnations_never_cover_each_other() {
        let before = version("one", &[(1, 3), (2, 5)]);
        let after = version("two", &[(1, 3), (2, 5)]);
        assert!(!before.covers(&after));
        assert!(!after.covers(&before));
        assert!(!version("two", &[(1, 9)]).covers(&version("one", &[])));
    }

    #[test]
    fn minted_incarnations_differ() {
        assert_ne!(new_incarnation(), new_incarnation());
    }
}
