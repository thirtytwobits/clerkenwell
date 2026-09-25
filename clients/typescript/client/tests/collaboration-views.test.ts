/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Replica views: a text field edited away from its replica, such as on another
 * thread, trading operations with it over an asynchronous link.
 */
import assert from "node:assert/strict";
import test from "node:test";

import type { TextBindingChange } from "@clerkenwell/client";
import { CollaborationTextView } from "@clerkenwell/client/loro";

import { NOTE_PLAN, noteDocument } from "./support/plans";
import { noteReplica, type NoteReplica } from "./support/replicas";
import { insert, replaceAll } from "./support/text";

/** A view whose messages to and from its replica wait until they are pumped. */
interface LinkedView {
  view: CollaborationTextView;
  /** Deliver every queued message, in order, until none remain. */
  pump: () => Promise<void>;
  detach: () => void;
  delivered: { toReplica: number; toView: number };
}

function link(replica: NoteReplica, fieldPath: "body" | "summary" = "body"): LinkedView {
  const toReplica: Uint8Array[] = [];
  const toView: Uint8Array[] = [];
  const delivered = { toReplica: 0, toView: 0 };
  const attached = replica.attachView((update) => toView.push(update));
  const view = new CollaborationTextView({
    entityName: "Note",
    plan: NOTE_PLAN,
    fieldPath,
    snapshot: attached.snapshot,
    publish: (update) => toReplica.push(update)
  });
  return {
    view,
    delivered,
    detach: () => attached.detach(),
    pump: async () => {
      // A view publishes its edits once the input transaction has finished.
      await Promise.resolve();
      while (toReplica.length > 0 || toView.length > 0) {
        const update = toReplica.shift();
        if (update !== undefined) {
          attached.receive(update);
          delivered.toReplica += 1;
        }
        const back = toView.shift();
        if (back !== undefined) {
          view.receive(back);
          delivered.toView += 1;
        }
        await Promise.resolve();
      }
    }
  };
}

test("a view starts from its replica's text and its edits reach the replica", async () => {
  const replica = noteReplica(noteDocument());
  const linked = link(replica);
  assert.equal(linked.view.binding.read(), noteDocument().body);

  insert(linked.view.binding, 0, "Minuted: ");
  await linked.pump();

  assert.equal(replica.currentDocument().body, linked.view.binding.read());
  linked.view.dispose();
});

test("a view's edits reach the replica's bindings and its other views, but never come back", async () => {
  const replica = noteReplica(noteDocument());
  const binding = replica.bindText("body");
  const author = link(replica);
  const reader = link(replica);

  insert(author.view.binding, 0, "Draft ");
  await author.pump();
  await reader.pump();

  assert.equal(binding.read(), author.view.binding.read());
  assert.equal(reader.view.binding.read(), author.view.binding.read());
  assert.equal(author.delivered.toView, 0);
  author.view.dispose();
  reader.view.dispose();
  replica.disposeTextBindings();
});

test("operations the replica takes reach a view as remote ranges", async () => {
  const replica = noteReplica(noteDocument());
  const peer = replica.fork();
  const linked = link(replica);
  const changes: TextBindingChange[] = [];
  linked.view.binding.subscribe((change) => changes.push(change));
  const frontier = replica.acceptedFrontierBase64();

  insert(peer.bindText("body"), 0, "Elsewhere: ");
  replica.importUpdateBase64(peer.exportIncrementalUpdateBase64(frontier));
  await linked.pump();

  assert.equal(linked.view.binding.read(), replica.currentDocument().body);
  assert.deepEqual(changes.map((change) => change.origin), ["remote"]);
  assert.deepEqual(changes[0]?.changes, [{ from: 0, to: 0, insert: "Elsewhere: " }]);
  linked.view.dispose();
  peer.disposeTextBindings();
});

test("a whole-document write reaches a view", async () => {
  const replica = noteReplica(noteDocument());
  const linked = link(replica);
  const rewritten = "Rewritten from the document.";

  replica.replaceDocument({ ...replica.currentDocument(), body: rewritten });
  await linked.pump();

  assert.equal(linked.view.binding.read(), rewritten);
  linked.view.dispose();
});

test("concurrent edits in a view and its replica converge", async () => {
  const replica = noteReplica(noteDocument());
  const peer = replica.fork();
  const linked = link(replica);
  const frontier = replica.acceptedFrontierBase64();

  insert(linked.view.binding, 0, "View. ");
  insert(peer.bindText("body"), peer.currentDocument().body.length, " Peer.");
  replica.importUpdateBase64(peer.exportIncrementalUpdateBase64(frontier));
  await linked.pump();

  const text = linked.view.binding.read();
  assert.equal(replica.currentDocument().body, text);
  assert.ok(text.startsWith("View. ") && text.endsWith(" Peer."));
  linked.view.dispose();
  peer.disposeTextBindings();
});

test("a view's frontier is one its replica holds, once received, at the text the view showed", async () => {
  const replica = noteReplica(noteDocument());
  const linked = link(replica);

  insert(linked.view.binding, 0, "Captured. ");
  const text = linked.view.binding.read();
  const frontier = linked.view.frontierBase64();
  insert(linked.view.binding, text.length, "Typed after capture.");
  await linked.pump();

  assert.ok(replica.coversFrontierBase64(frontier));
  assert.equal(replica.documentAt(frontier).body, text);
  assert.notEqual(linked.view.frontierBase64(), frontier);
  linked.view.dispose();
});

test("a composing view takes the replica's operations once its composition ends", async () => {
  const replica = noteReplica(noteDocument());
  const peer = replica.fork();
  const linked = link(replica);
  const frontier = replica.acceptedFrontierBase64();
  const before = linked.view.binding.read();

  const release = linked.view.binding.beginComposition();
  insert(peer.bindText("body"), 0, "Remote. ");
  replica.importUpdateBase64(peer.exportIncrementalUpdateBase64(frontier));
  await linked.pump();
  assert.equal(linked.view.binding.read(), before);

  release();
  assert.equal(linked.view.binding.read(), replica.currentDocument().body);
  linked.view.dispose();
  peer.disposeTextBindings();
});

test("a view's undo takes back only its own edits", async () => {
  const replica = noteReplica({ ...noteDocument(), body: "" });
  const peer = replica.fork();
  const linked = link(replica);
  const frontier = replica.acceptedFrontierBase64();

  insert(linked.view.binding, 0, "Mine.");
  await linked.pump();
  insert(peer.bindText("body"), 0, "Theirs. ");
  replica.importUpdateBase64(peer.exportIncrementalUpdateBase64(frontier));
  await linked.pump();
  const end = linked.view.binding.read().length;
  linked.view.binding.undo({ anchor: end, head: end });
  await linked.pump();

  assert.equal(linked.view.binding.read(), "Theirs. ");
  assert.equal(replica.currentDocument().body, "Theirs. ");
  linked.view.dispose();
  peer.disposeTextBindings();
});

test("a view edits one field and follows every other field it holds", async () => {
  const replica = noteReplica(noteDocument());
  const summary = link(replica, "summary");
  const body = link(replica, "body");

  replaceAll(body.view.binding, "Body from its own view.");
  await body.pump();
  await summary.pump();

  assert.equal(replica.currentDocument().body, "Body from its own view.");
  assert.equal(summary.view.binding.read(), noteDocument().summary);
  insert(summary.view.binding, 0, "Short: ");
  await summary.pump();
  assert.equal(replica.currentDocument().summary, summary.view.binding.read());
  summary.view.dispose();
  body.view.dispose();
});

test("a detached view receives nothing more from its replica", async () => {
  const replica = noteReplica(noteDocument());
  const linked = link(replica);
  linked.detach();

  replica.replaceDocument({ ...replica.currentDocument(), body: "After detaching." });
  await linked.pump();

  assert.equal(linked.delivered.toView, 0);
  assert.equal(linked.view.binding.read(), noteDocument().body);
  linked.view.dispose();
});

test("a replica takes nothing from a view that sends only what it holds", async () => {
  const replica = noteReplica(noteDocument());
  const attached = replica.attachView(() => undefined);

  assert.equal(attached.receive(attached.snapshot), false);
  attached.detach();
});
