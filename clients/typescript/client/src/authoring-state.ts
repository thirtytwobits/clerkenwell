/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The authoring state: the projection a client keeps a replica of a
 * collaborative document in step with. Every collaborative entity's authoring
 * state delivers these types, and generated bindings re-export them. They
 * are type aliases, as generated types are, so they stand wherever a JSON
 * object does.
 */

/** Whether an import creates a document (`bootstrap`) or edits one (`incremental`). */
export type CollaborationExchangeMode = "incremental" | "bootstrap";

/**
 * A block of peers allocated to the subscriber: every peer whose bits above
 * the low `index_bits` are `base`'s. An import names the block by its nonce.
 */
export type AuthoringPeerBlock = {
  nonce: string;
  /** The block's first peer, in decimal. */
  base: string;
  index_bits: number;
};

/** A document's accepted state as an authoring-state update delivers it. */
export type AuthoringState = {
  /** The layout version the document's operations are written in. */
  schema_version: number;
  /** The frontier of the accepted state the update leaves its client holding. */
  accepted_frontier_base64: string;
  /**
   * The accepted operations the update adds: every one in a snapshot, and in
   * a patch those after the frontier its client held.
   */
  update_base64: string;
  /** The accepted state's etag. */
  etag: string;
  /** How the store accepts an import of the document. */
  exchange_modes: CollaborationExchangeMode[];
  /** A new block of peers for the client to write the document under. */
  peer_block: AuthoringPeerBlock;
  /**
   * Whether the client replaces what it holds with this state, because what
   * it named holding is not of this document's history.
   */
  replaces_held: boolean;
};

/** An authoring-state patch: the document's new accepted state, or its removal. */
export type AuthoringStatePatch = {
  kind: "replace" | "remove";
  /** The state a replacement delivers. */
  state?: AuthoringState;
};
