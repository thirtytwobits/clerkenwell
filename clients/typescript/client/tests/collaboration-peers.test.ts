/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A replica writes under the peers of the block a server allocated it, and
 * every replica forked from it, text binding and view takes another peer of
 * that block.
 */
import assert from "node:assert/strict";
import test from "node:test";

import { CollaborationReplica, type CollaborationPeerBlock } from "@clerkenwell/client/replica";
import { decodeImportBlobMeta } from "loro-crdt";

import { base64ToBytes } from "../src/binary.js";
import { peerBlock } from "./support/peers.js";
import { NOTE_PLAN, noteDocument, type NoteDocument } from "./support/plans.js";
import { insert } from "./support/text.js";

/** Every peer an update holds operations of, in decimal. */
function peersIn(updateBase64: string): string[] {
  const meta = decodeImportBlobMeta(base64ToBytes(updateBase64), false);
  return [...meta.partialEndVersionVector.toJSON().keys()].sort();
}

function peer(block: CollaborationPeerBlock, index: number): string {
  return (BigInt(block.base) + BigInt(index)).toString();
}

function noteReplica(block: CollaborationPeerBlock): CollaborationReplica<NoteDocument> {
  return CollaborationReplica.from("Note", NOTE_PLAN, { kind: "document", document: noteDocument() }, block);
}

test("a replica writes under the first peer of its block", () => {
  const block = peerBlock();
  const replica = noteReplica(block);

  assert.deepEqual(peersIn(replica.exportUpdateBase64()), [peer(block, 0)]);
  assert.deepEqual(replica.peerNonces(), [block.nonce]);
});

test("forks, text bindings and views each take the next peer of the block", () => {
  const block = peerBlock();
  const replica = noteReplica(block);
  const base = replica.acceptedFrontierBase64();

  const fork = replica.fork();
  fork.replaceDocument({ ...fork.currentDocument(), title: "Forked" });
  replica.importUpdateBase64(fork.exportIncrementalUpdateBase64(base));
  const binding = replica.bindText("body");
  insert(binding, 0, "Bound. ");
  const view = replica.attachView(() => undefined);

  assert.deepEqual(
    peersIn(replica.exportIncrementalUpdateBase64(base)).sort(),
    [peer(block, 1), peer(block, 2)].sort()
  );
  assert.equal(view.peer, peer(block, 3));
  assert.deepEqual(fork.peerNonces(), [block.nonce]);
});

test("forks of a replica that adopted a new block write under the new block", () => {
  const first = peerBlock();
  const second = peerBlock();
  const replica = noteReplica(first);
  const base = replica.acceptedFrontierBase64();

  replica.adoptPeerBlock(second);
  const fork = replica.fork();
  fork.replaceDocument({ ...fork.currentDocument(), title: "Forked" });

  assert.deepEqual(peersIn(fork.exportIncrementalUpdateBase64(base)), [peer(second, 0)]);
  assert.deepEqual([...replica.peerNonces()].sort(), [first.nonce, second.nonce].sort());
});

test("a block adopted again goes on from the peers it has given", () => {
  const first = peerBlock();
  const second = peerBlock();
  const replica = noteReplica(first);
  const base = replica.acceptedFrontierBase64();
  const writers: CollaborationReplica<NoteDocument>[] = [];

  replica.adoptPeerBlock(first);
  writers.push(replica.fork());
  replica.adoptPeerBlock(second);
  writers.push(replica.fork());
  replica.adoptPeerBlock(first);
  writers.push(replica.fork());

  const peers = writers.map((writer) => {
    writer.replaceDocument({ ...writer.currentDocument(), title: "Forked" });
    return peersIn(writer.exportIncrementalUpdateBase64(base));
  });
  assert.deepEqual(peers, [[peer(first, 1)], [peer(second, 0)], [peer(first, 2)]]);
});

test("a view the block has no peer left for is not attached", () => {
  const block: CollaborationPeerBlock = { ...peerBlock(), index_bits: 0 };
  const replica = noteReplica(block);
  let sent = 0;

  assert.throws(() => replica.attachView(() => { sent += 1; }), /adopt a new block/);
  replica.replaceDocument({ ...replica.currentDocument(), title: "Renamed" });

  assert.equal(sent, 0);
});

test("a replica refuses to write under more peers than its block holds", () => {
  const block: CollaborationPeerBlock = { ...peerBlock(), index_bits: 1 };
  const replica = noteReplica(block);

  replica.fork();
  assert.throws(() => replica.fork(), /adopt a new block/);
});

test("a replica names the blocks of operations it was given as well as its own", () => {
  const block = peerBlock();
  const replica = noteReplica(block);

  replica.includePeerNonces(["restored"]);

  assert.deepEqual([...replica.peerNonces()].sort(), [block.nonce, "restored"].sort());
});
