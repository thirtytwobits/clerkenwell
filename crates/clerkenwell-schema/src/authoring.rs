//! The authoring state: the projection a client keeps a replica of a
//! collaborative document in step with.
//!
//! Every collaborative entity's authoring state delivers these types. The
//! generator declares their schemas in every definition with a collaborative
//! entity, and generated bindings re-export these types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A document's accepted state as an authoring-state update delivers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthoringState {
    /// The layout version the document's operations are written in.
    pub schema_version: u32,
    /// The frontier of the accepted state the update leaves its client holding.
    pub accepted_frontier_base64: String,
    /// The accepted operations the update adds: every one in a snapshot, and
    /// in a patch those after the frontier its client held.
    pub update_base64: String,
    /// The accepted state's etag.
    pub etag: String,
    /// How the store accepts an import of the document.
    pub exchange_modes: Vec<CollaborationExchangeMode>,
    /// A new block of peers for the client to write the document under.
    pub peer_block: AuthoringPeerBlock,
    /// Whether the client replaces what it holds with this state, because
    /// what it named holding is not of this document's history.
    pub replaces_held: bool,
}

/// A block of peers allocated to the subscriber: every peer whose bits above
/// the low `index_bits` are `base`'s. An import names the block by its nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthoringPeerBlock {
    pub nonce: String,
    /// The block's first peer, in decimal.
    pub base: String,
    pub index_bits: u32,
}

/// An authoring-state patch: the document's new accepted state, or its
/// removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthoringStatePatch {
    pub kind: AuthoringStatePatchKind,
    /// The state a replacement delivers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AuthoringState>,
}

impl AuthoringStatePatch {
    /// The patch delivering `state` in place of what the client holds.
    pub fn replace(state: AuthoringState) -> Self {
        Self {
            kind: AuthoringStatePatchKind::Replace,
            state: Some(state),
        }
    }

    /// The patch removing the document.
    pub fn remove() -> Self {
        Self {
            kind: AuthoringStatePatchKind::Remove,
            state: None,
        }
    }
}

/// What an authoring-state patch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuthoringStatePatchKind {
    Replace,
    Remove,
}

/// Whether an import creates a document (`Bootstrap`) or edits one
/// (`Incremental`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CollaborationExchangeMode {
    Incremental,
    Bootstrap,
}
