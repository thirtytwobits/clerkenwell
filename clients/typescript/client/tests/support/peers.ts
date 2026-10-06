/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Peer blocks for replicas the tests create, as a server allocates them.
 */
import type { CollaborationPeerBlock } from "@clerkenwell/client/replica";

const INDEX_BITS = 16;
let nextBlock = 1n;

/** A block of peers no other replica of the test writes under. */
export function peerBlock(): CollaborationPeerBlock {
  const base = nextBlock << BigInt(INDEX_BITS);
  nextBlock += 1n;
  return { nonce: `block-${base}`, base: base.toString(), index_bits: INDEX_BITS };
}
