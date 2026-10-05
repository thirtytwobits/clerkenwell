//! Peer blocks: the peers a principal may write a document's operations under.
//!
//! A block is every peer whose high bits are the block's base. The base is a
//! keyed hash of the principal, the document and a nonce, so only the store
//! set holding the key can name a principal's block, and allocating one
//! writes nothing. A writer chooses the low [`PEER_INDEX_BITS`] bits, one
//! value for each replica it writes from.

use std::fmt;
use std::sync::Arc;

use clerkenwell_events::{ActorKind, Principal};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use uuid::Uuid;

use crate::{CollaborationDocumentId, StoreError, StoreResult};

/// The low bits of a peer its writer chooses within its block.
pub const PEER_INDEX_BITS: u32 = 16;

/// The bits of a block's base the key derives. The peer's top bit stays
/// clear, so no peer is the value the replicas reserve.
const DERIVED_BITS: u32 = 63 - PEER_INDEX_BITS;

/// The fewest bytes a [`PeerKey`] holds.
pub const PEER_KEY_MIN_BYTES: usize = 32;

/// The secret a store set derives peer blocks from. The application keeps it
/// as configuration; a new key leaves unused allocations unusable and every
/// recorded binding as it was.
#[derive(Clone)]
pub struct PeerKey(Arc<[u8]>);

impl PeerKey {
    /// The key `secret` is, which holds at least [`PEER_KEY_MIN_BYTES`].
    pub fn new(secret: impl AsRef<[u8]>) -> StoreResult<Self> {
        let secret = secret.as_ref();
        if secret.len() < PEER_KEY_MIN_BYTES {
            return Err(StoreError::invalid_request(format!(
                "A peer key holds at least {PEER_KEY_MIN_BYTES} bytes."
            ))
            .with_data(serde_json::json!({
                "code": "invalid_peer_key",
                "min_bytes": PEER_KEY_MIN_BYTES,
            })));
        }
        Ok(Self(secret.into()))
    }

    /// A new block of peers for `principal` to write `document` under.
    pub(crate) fn allocate(
        &self,
        principal: &Principal,
        document: &CollaborationDocumentId,
    ) -> PeerBlock {
        let nonce = Uuid::new_v4().simple().to_string();
        let base = self.base(principal, document, &nonce);
        PeerBlock { nonce, base }
    }

    /// The base of the block `nonce` names for `principal` and `document`.
    pub(crate) fn base(
        &self,
        principal: &Principal,
        document: &CollaborationDocumentId,
        nonce: &str,
    ) -> u64 {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0)
            .expect("HMAC accepts a key of any length");
        for field in [
            kind_name(principal.kind),
            principal.id.as_str(),
            document.entity.as_str(),
            document.resource_id.as_str(),
            nonce,
        ] {
            mac.update(&(field.len() as u64).to_be_bytes());
            mac.update(field.as_bytes());
        }
        let digest = mac.finalize().into_bytes();
        let mut head = [0; 8];
        head.copy_from_slice(&digest[..8]);
        (u64::from_be_bytes(head) >> (64 - DERIVED_BITS)) << PEER_INDEX_BITS
    }
}

impl fmt::Debug for PeerKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PeerKey(..)")
    }
}

/// A block of peers allocated to one principal for one document: every peer
/// whose bits above the low [`PEER_INDEX_BITS`] are `base`'s. An import names
/// the block by its nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerBlock {
    pub nonce: String,
    pub base: u64,
}

/// The base of the block `peer` is in.
pub(crate) fn block_of(peer: u64) -> u64 {
    peer >> PEER_INDEX_BITS << PEER_INDEX_BITS
}

fn kind_name(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::Service => "service",
        ActorKind::System => "system",
    }
}
