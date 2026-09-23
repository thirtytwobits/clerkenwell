/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Text bindings: synchronous local edits, remote edits as ranges, field-scoped
 * undo with selections, composition gating, keyed targets and staged branches.
 */
import assert from "node:assert/strict";
import test from "node:test";

import type { TextBindingChange } from "@clerkenwell/client";

import { boardDocument, noteDocument } from "./support/plans";
import {
  boardReplica,
  boardReplicaFromUpdate,
  noteReplica,
  type NoteReplica
} from "./support/replicas";
import { insert } from "./support/text";

const initialText = "Before 🦊 after";
const secondText = "Second field";

function replicas(): { a: NoteReplica; b: NoteReplica } {
  const seed = noteReplica({ ...noteDocument(), body: initialText, summary: secondText });
  return { a: seed.fork(), b: seed.fork() };
}

test("undo after an accepted first edit restores the empty text and its caret", () => {
  const draft = noteReplica({ ...noteDocument(), body: "" });
  const peer = draft.fork();
  const field = draft.bindText("body");
  insert(field, 0, "First thought 🦊");
  peer.importUpdateBase64(draft.exportUpdateBase64());
  draft.importUpdateBase64(peer.exportUpdateBase64());

  const selection = field.undo({ anchor: field.read().length, head: field.read().length });

  assert.equal(field.read(), "");
  assert.deepEqual(selection, { anchor: 0, head: 0, affinity: 1 });
  draft.disposeTextBindings();
});

test("local edits are synchronous and an echoed update produces no editor change", () => {
  const { a, b } = replicas();
  const field = a.bindText("body");
  const events: TextBindingChange[] = [];
  field.subscribe((event) => events.push(event));
  const at = initialText.indexOf("after");
  const text = "new ";

  insert(field, at, text);

  assert.equal(field.read(), initialText.slice(0, at) + text + initialText.slice(at));
  assert.equal(events.length, 1);
  assert.deepEqual(events[0]?.changes, [{ from: at, to: at, insert: text }]);
  assert.equal(events[0]?.origin, "local");
  b.importUpdateBase64(a.exportUpdateBase64());
  a.importUpdateBase64(b.exportUpdateBase64());
  assert.equal(events.length, 1);
  assert.equal(a.currentDocument().body, field.read());
});

test("a remote edit arrives as ranges with a remote origin", () => {
  const { a, b } = replicas();
  const field = a.bindText("body");
  const events: TextBindingChange[] = [];
  field.subscribe((event) => events.push(event));
  const prefix = "remote ";

  insert(b.bindText("body"), 0, prefix);
  a.importUpdateBase64(b.exportUpdateBase64());

  assert.equal(events.length, 1);
  assert.equal(events[0]?.origin, "remote");
  assert.deepEqual(events[0]?.changes, [{ from: 0, to: 0, insert: prefix }]);
  assert.equal(field.read(), prefix + initialText);
});

test("concurrent edits merge while newer unacknowledged local edits remain", () => {
  const { a, b } = replicas();
  const left = a.bindText("body");
  const right = b.bindText("body");
  insert(left, 0, "left ");
  const olderUpdate = a.exportUpdateBase64();
  insert(left, left.read().length, " newer");
  insert(right, right.read().length, " remote");

  b.importUpdateBase64(olderUpdate);
  a.importUpdateBase64(b.exportUpdateBase64());
  b.importUpdateBase64(a.exportUpdateBase64());

  assert.equal(left.read(), right.read());
  for (const contribution of ["left ", " newer", " remote", initialText]) {
    assert.ok(left.read().includes(contribution), contribution);
  }
});

test("undo is local, field scoped and ignores another field's history", () => {
  const { a, b } = replicas();
  const first = a.bindText("body");
  const second = a.bindText("summary");
  insert(first, 0, "mine ");
  insert(second, 0, "other ");
  insert(b.bindText("body"), initialText.length, " theirs");
  a.importUpdateBase64(b.exportUpdateBase64());

  first.undo({ anchor: 5, head: 5 });
  assert.equal(first.read(), initialText + " theirs");
  assert.equal(second.read(), "other " + secondText);
  second.undo({ anchor: 6, head: 6 });
  assert.equal(second.read(), secondText);
  assert.equal(first.read(), initialText + " theirs");
  first.redo({ anchor: 0, head: 0 });
  second.redo({ anchor: 0, head: 0 });
  assert.ok(first.read().startsWith("mine "));
  assert.equal(second.read(), "other " + secondText);
});

test("undo and redo report a history origin and restore the selection", () => {
  const { a } = replicas();
  const field = a.bindText("body");
  const events: TextBindingChange[] = [];
  field.subscribe((event) => events.push(event));
  const at = initialText.indexOf("after");
  const selectionBefore = { anchor: at, head: at, affinity: -1 as const };
  field.edit({
    baseRevision: field.revision,
    changes: [{ from: at, to: at, insert: "local " }],
    selectionBefore,
    selectionAfter: { anchor: at + 6, head: at + 6 },
    group: "typing"
  });

  assert.deepEqual(field.undo({ anchor: at + 6, head: at + 6 }), selectionBefore);
  assert.equal(field.read(), initialText);
  assert.equal(events.at(-1)?.origin, "history");
  assert.notEqual(field.redo({ anchor: at, head: at }), null);
  assert.equal(field.read(), initialText.slice(0, at) + "local " + initialText.slice(at));
});

test("a composition keeps its base until release, then integrates remote edits", () => {
  const { a, b } = replicas();
  const field = a.bindText("body");
  const finish = field.beginComposition();
  const revision = field.revision;
  insert(b.bindText("body"), 0, "remote ");
  a.importUpdateBase64(b.exportUpdateBase64());
  assert.equal(field.revision, revision);
  assert.equal(field.read(), initialText);

  insert(field, initialText.length, " 語", "composition");
  finish();
  finish();

  assert.equal(field.read(), "remote " + initialText + " 語");
  b.importUpdateBase64(a.exportUpdateBase64());
  assert.equal(field.read(), b.bindText("body").read());
});

test("cancelling a composition integrates remote changes without creating local undo", () => {
  const { a, b } = replicas();
  const field = a.bindText("body");
  const finish = field.beginComposition();
  insert(b.bindText("body"), 0, "remote ");
  a.importUpdateBase64(b.exportUpdateBase64());
  finish();

  assert.equal(field.undo({ anchor: 0, head: 0 }), null);
  assert.equal(field.read(), "remote " + initialText);
});

test("a composition delays only its field while other fields and the document keep synchronising", () => {
  const { a, b } = replicas();
  const composing = a.bindText("body");
  const other = a.bindText("summary");
  const finish = composing.beginComposition();
  insert(b.bindText("body"), 0, "remote ");
  insert(b.bindText("summary"), 0, "independent ");
  a.importUpdateBase64(b.exportUpdateBase64());

  assert.equal(composing.read(), initialText);
  assert.equal(other.read(), "independent " + secondText);
  assert.equal(a.currentDocument().body, "remote " + initialText);
  insert(composing, initialText.length, "語", "composition");
  finish();
  assert.equal(composing.read(), "remote " + initialText + "語");
  assert.equal(a.currentDocument().body, composing.read());
  b.importUpdateBase64(a.exportUpdateBase64());
  assert.equal(b.bindText("body").read(), composing.read());
});

test("a replica cannot close while one of its fields is composing", () => {
  const { a } = replicas();
  const finish = a.bindText("body").beginComposition();

  assert.throws(() => a.disposeTextBindings(), /composition/);
  finish();
  assert.doesNotThrow(() => a.disposeTextBindings());
});

test("out-of-order incremental imports reach a composing field once their dependencies arrive", () => {
  const { a, b } = replicas();
  const local = a.bindText("body");
  const remote = b.bindText("body");
  const base = b.acceptedFrontierBase64();
  const prefix = "first ";
  const suffix = " second";
  insert(remote, 0, prefix);
  const first = b.exportIncrementalUpdateBase64(base);
  const middle = b.acceptedFrontierBase64();
  insert(remote, remote.read().length, suffix);
  const second = b.exportIncrementalUpdateBase64(middle);

  const release = local.beginComposition();
  a.importUpdateBase64(second);
  a.importUpdateBase64(first);
  const expected = prefix + initialText + suffix;
  assert.equal(a.currentDocument().body, expected);
  assert.equal(local.read(), initialText);
  release();
  assert.equal(local.read(), expected);

  const events: TextBindingChange[] = [];
  local.subscribe((event) => events.push(event));
  a.importUpdateBase64(first);
  a.importUpdateBase64(second);
  assert.deepEqual(events, []);

  const own = " local";
  insert(local, local.read().length, own);
  b.importUpdateBase64(a.exportIncrementalUpdateBase64(b.acceptedFrontierBase64()));
  assert.equal(remote.read(), expected + own);
  local.undo({ anchor: local.read().length, head: local.read().length });
  assert.equal(local.read(), expected);
});

test("stale or invalid transactions fail before changing any text", () => {
  const { a } = replicas();
  const field = a.bindText("body");
  const revision = field.revision;
  insert(field, 0, "new ");
  const unchanged = field.read();
  const edit = {
    baseRevision: revision,
    changes: [{ from: 0, to: 0, insert: "old " }],
    selectionBefore: { anchor: 0, head: 0 },
    selectionAfter: { anchor: 4, head: 4 },
    group: "typing"
  };

  assert.throws(() => field.edit(edit), /revision/i);
  assert.throws(() => field.edit({
    ...edit,
    baseRevision: field.revision,
    changes: [...edit.changes, { from: unchanged.length + 1, to: unchanged.length + 1, insert: "invalid" }]
  }), /range/i);
  assert.throws(() => field.edit({
    ...edit,
    baseRevision: field.revision,
    selectionAfter: { anchor: unchanged.length + 99, head: 0 }
  }), /selection/i);
  assert.equal(field.read(), unchanged);
});

test("an edit may not split a Unicode character", () => {
  const replica = noteReplica({ ...noteDocument(), body: "🦊🦊" });
  const field = replica.bindText("body");
  const between = "🦊".length;
  const text = " and ";

  insert(field, between, text);
  const accepted = field.read();
  assert.equal(accepted, "🦊" + text + "🦊");
  assert.throws(() => insert(field, 1, text), /Unicode/);
  assert.equal(field.read(), accepted);
  replica.disposeTextBindings();
});

test("only declared text fields bind", () => {
  const { a } = replicas();

  assert.throws(() => a.bindText("title"), /text field/);
  assert.throws(() => a.bindText("missing"), /text field/);
  assert.equal(a.bindText("body"), a.bindText("body"));
});

test("changes in another field neither notify nor invalidate a binding", () => {
  const { a } = replicas();
  const field = a.bindText("body");
  let notifications = 0;
  field.subscribe(() => notifications++);
  const revision = field.revision;

  insert(a.bindText("summary"), 0, "separate ");

  assert.equal(field.revision, revision);
  assert.equal(notifications, 0);
});

test("a whole-document write reaches bound fields as an edit", () => {
  const { a } = replicas();
  const field = a.bindText("body");
  const events: TextBindingChange[] = [];
  field.subscribe((event) => events.push(event));
  const body = "Rewritten " + initialText;

  a.replaceDocument({ ...a.currentDocument(), body });

  assert.equal(field.read(), body);
  assert.equal(events.length, 1);
});

test("a binding exports its edits incrementally over the accepted frontier", () => {
  const seed = noteReplica(noteDocument());
  const authoring = seed.fork();
  const accepted = seed.fork();
  const body = authoring.bindText("body");
  const insertion = "Before that, ";

  insert(body, 0, insertion);
  accepted.importUpdateBase64(authoring.exportIncrementalUpdateBase64(accepted.acceptedFrontierBase64()));

  assert.equal(accepted.currentDocument().body, insertion + noteDocument().body);
  assert.equal(authoring.currentDocument().body, body.read());
});

test("a keyed item's text binding follows its identity through reorder and becomes unavailable on deletion", () => {
  const seed = boardReplica(boardDocument());
  const local = seed.fork();
  const remote = seed.fork();
  const task = boardDocument().columns[0]!.tasks[1]!;
  const notes = local.bindText("columns.*.tasks.*.notes", { column_id: "todo", task_id: task.id });
  const availability: boolean[] = [];
  notes.subscribe((change) => availability.push(change.available));

  const reordered = structuredClone(remote.currentDocument());
  reordered.columns.reverse();
  reordered.columns[1]!.tasks.reverse();
  remote.replaceDocument(reordered);
  local.importUpdateBase64(remote.exportUpdateBase64());
  assert.equal(notes.read(), task.notes);
  assert.equal(notes.available, true);

  remote.importUpdateBase64(local.exportUpdateBase64());
  const removed = structuredClone(remote.currentDocument());
  for (const column of removed.columns) {
    column.tasks = column.tasks.filter(({ id }) => id !== task.id);
  }
  remote.replaceDocument(removed);
  local.importUpdateBase64(remote.exportUpdateBase64());

  assert.equal(notes.available, false);
  assert.equal(availability.at(-1), false);
  assert.throws(() => insert(notes, 0, "too late"), /no longer available/);
});

test("a root text binding survives deleting unrelated keyed items", () => {
  const seed = boardReplica(boardDocument());
  const local = seed.fork();
  const remote = seed.fork();
  const description = local.bindText("description");

  remote.replaceDocument({ ...remote.currentDocument(), columns: [] });
  local.importUpdateBase64(remote.exportUpdateBase64());

  assert.equal(description.available, true);
  assert.equal(description.read(), boardDocument().description);
});

test("a keyed text binding needs every enclosing identity and a present item", () => {
  const replica = boardReplica(boardDocument());

  assert.throws(() => replica.bindText("columns.*.tasks.*.notes", { column_id: "todo" }), /task_id/);
  assert.throws(
    () => replica.bindText("columns.*.tasks.*.notes", { column_id: "todo", task_id: "missing" }),
    /missing record/
  );
});

test("a staged branch stays private, merges on confirmation, and survives its target's deletion", () => {
  const seed = boardReplica(boardDocument());
  const update = seed.exportUpdateBase64();
  const local = boardReplicaFromUpdate(update);
  const remote = boardReplicaFromUpdate(update);
  const target = { column_id: "todo", task_id: "task-1" };
  const stage = local.stageText("columns.*.tasks.*.notes", target);
  const original = stage.binding.read();
  const stagedSuffix = "\n\nA private staged addition.";
  insert(stage.binding, original.length, stagedSuffix);
  assert.equal(local.currentDocument().columns[0]!.tasks[0]!.notes, original);

  const remoteNotes = remote.bindText("columns.*.tasks.*.notes", target);
  const remotePrefix = "Remote: ";
  insert(remoteNotes, 0, remotePrefix);
  local.importUpdateBase64(remote.exportUpdateBase64());

  assert.deepEqual(stage.confirm(), { kind: "committed" });
  assert.equal(
    local.currentDocument().columns[0]!.tasks[0]!.notes,
    remotePrefix + original + stagedSuffix
  );
  stage.dispose();

  const blocked = local.stageText("columns.*.tasks.*.notes", target);
  const retainedDraft = blocked.binding.read();
  remote.importUpdateBase64(local.exportUpdateBase64());
  const emptied = structuredClone(remote.currentDocument());
  emptied.columns[0]!.tasks = [];
  remote.replaceDocument(emptied);
  local.importUpdateBase64(remote.exportUpdateBase64());

  const confirmation = blocked.confirm();
  assert.equal(confirmation.kind, "blocked");
  assert.equal(confirmation.kind === "blocked" ? confirmation.reason : undefined, "targetDeleted");
  assert.equal(blocked.binding.read(), retainedDraft);
  blocked.dispose();
  assert.throws(() => blocked.confirm(), /closed/);
});

test("sustained editing keeps undo selections after history eviction", () => {
  const replica = noteReplica({ ...noteDocument(), body: "" });
  const accepted = replica.fork();
  const field = replica.bindText("body");
  const text = "thought 🦊 ";
  for (let index = 0; index < 250; index += 1) {
    insert(field, field.read().length, text, String(index));
    accepted.importUpdateBase64(replica.exportUpdateBase64());
    replica.importUpdateBase64(accepted.exportUpdateBase64());
  }

  for (let index = 0; index < 50; index += 1) {
    const before = field.read();
    const selection = field.undo({ anchor: before.length, head: before.length });
    const expected = before.slice(0, -text.length);
    assert.equal(field.read(), expected);
    assert.deepEqual(selection, { anchor: expected.length, head: expected.length, affinity: 1 });
  }
  replica.disposeTextBindings();
  accepted.disposeTextBindings();
});
