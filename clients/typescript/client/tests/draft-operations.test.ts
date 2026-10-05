/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A replica takes a persisted collaborative draft as the operations that made
 * it, so restoring pending work never authors those edits again.
 */
import assert from "node:assert/strict";
import test from "node:test";
import { peerBlock } from "./support/peers.js";

import {
  AuthoringRuntime,
  fromPersistedRuntime,
  toPersistedRuntime,
  type AuthoringRuntimeState
} from "@clerkenwell/client";
import type { AuthoringSessionHandle } from "../src/authoring-runtime.js";
import type { AuthoringSessionController } from "../src/authoring-session.js";

import { FakeBoardServer, openBoardSession } from "./support/board-server.js";
import { boardDocument, type BoardDocument } from "./support/plans.js";
import { BOARD_DRAFTS, type BoardTextFieldPath } from "./support/replicas.js";
import { insert } from "./support/text.js";

const resource = { entity: "Board", resourceKey: "board-1" } as const;
const noted = { column_id: "todo", task_id: "task-1" } as const;

function notesOf(board: BoardDocument): string {
  return board.columns[0]?.tasks[0]?.notes ?? "";
}

function withNotes(board: BoardDocument, notes: string): BoardDocument {
  const next = structuredClone(board);
  const task = next.columns[0]?.tasks[0];
  assert.ok(task);
  task.notes = notes;
  return next;
}

function copiesOf(text: string, fragment: string): number {
  return text.split(fragment).length - 1;
}

async function append(session: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>, text: string): Promise<void> {
  const notes = session.bindText("columns.*.tasks.*.notes", noted);
  insert(notes, notes.read().length, text);
  await new Promise<void>((resolve) => queueMicrotask(resolve));
}

/** Deletes the last `count` characters of the notes as one typing step. */
async function trim(session: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>, count: number): Promise<void> {
  const notes = session.bindText("columns.*.tasks.*.notes", noted);
  const end = notes.read().length;
  notes.edit({
    baseRevision: notes.revision,
    changes: [{ from: end - count, to: end, insert: "" }],
    selectionBefore: { anchor: end, head: end },
    selectionAfter: { anchor: end - count, head: end - count },
    group: "typing"
  });
  await new Promise<void>((resolve) => queueMicrotask(resolve));
}

/** Sends a session's pending work and adopts what the server accepted. */
function save(server: FakeBoardServer, session: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>): void {
  const base = session.state().acceptedRevision;
  const result = server.accept({
    baseFrontierBase64: base,
    updateBase64: session.exportIncrementalUpdateBase64(base)
  });
  session.adoptAccepted({
    updateBase64: result.state.update_base64,
    acceptedFrontierBase64: result.state.accepted_frontier_base64,
    baseline: result.board,
    acceptedRevision: result.state.accepted_frontier_base64
  });
}

/** Sends a session's pending work and adopts a reply carrying only what the session lacks. */
function saveReceivingMissing(server: FakeBoardServer, session: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>): void {
  const base = session.state().acceptedRevision;
  const result = server.accept({
    baseFrontierBase64: base,
    updateBase64: session.exportIncrementalUpdateBase64(base)
  });
  const accepted = result.state.accepted_frontier_base64;
  session.adoptAccepted({
    updateBase64: result.missingUpdateBase64,
    acceptedFrontierBase64: accepted,
    acceptedRevision: accepted
  });
}

function persisted(runtime: AuthoringRuntime): AuthoringRuntimeState {
  return fromPersistedRuntime(toPersistedRuntime(runtime.getSnapshot()));
}

/** Attaches a replica built from what the server has accepted, as a reload does. */
function attachAccepted(runtime: AuthoringRuntime, server: FakeBoardServer): {
  session: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>;
  accepted: string;
} {
  const state = server.snapshot();
  return {
    session: runtime.ensureController(resource, () =>
      BOARD_DRAFTS.fromUpdate(state.update_base64, peerBlock()).controller()),
    accepted: state.accepted_frontier_base64
  };
}

/** Sends what a session's replica holds beyond an accepted frontier. */
function sync(
  server: FakeBoardServer,
  session: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>,
  accepted: string
): void {
  server.accept({
    baseFrontierBase64: accepted,
    updateBase64: session.exportIncrementalUpdateBase64(accepted)
  });
}

test("a save answered with only missing operations records later pending work from the accepted frontier", async () => {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  const session = openBoardSession(runtime, server, resource);
  await append(session, " saved");
  server.editRemotely((board) => withNotes(board, `Elsewhere ${notesOf(board)}`));

  saveReceivingMissing(server, session);
  assert.deepEqual(session.currentDraft(), server.board());
  assert.deepEqual(runtime.session(resource)?.baseline, server.board());

  await append(session, " pending");
  const operations = runtime.session(resource)?.draftOperations;
  assert.equal(operations?.baseFrontierBase64, server.snapshot().accepted_frontier_base64);
  const restored = BOARD_DRAFTS.fromUpdate(server.snapshot().update_base64, peerBlock());
  restored.importUpdateBase64(operations!.updateBase64);
  const notes = notesOf(restored.currentDraft());
  assert.equal(copiesOf(notes, " saved"), 1);
  assert.equal(copiesOf(notes, " pending"), 1);
  assert.equal(copiesOf(notes, "Elsewhere"), 1);
});

test("a restart takes pending edits as operations and keeps edits accepted meanwhile", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Offline.");
  const snapshot = persisted(beforeRestart);
  server.editRemotely((board) => withNotes(board, `Remote. ${notesOf(board)}`));

  const afterRestart = new AuthoringRuntime(snapshot);
  const { session, accepted } = attachAccepted(afterRestart, server);
  sync(server, session, accepted);

  const merged = notesOf(server.board());
  assert.equal(copiesOf(merged, " Offline."), 1, merged);
  assert.equal(copiesOf(merged, "Remote. "), 1, merged);
  assert.deepEqual(session.currentDraft(), server.board());
});

test("a runtime starting on an earlier snapshot keeps edits accepted since it was taken", async () => {
  const server = new FakeBoardServer(boardDocument());
  const typing = new AuthoringRuntime();
  const typed = openBoardSession(typing, server, resource);
  const accepted = server.snapshot().accepted_frontier_base64;
  await append(typed, " First.");
  const earlier = persisted(typing);
  await append(typed, " Second.");
  sync(server, typed, accepted);

  const attached = attachAccepted(new AuthoringRuntime(earlier), server);
  sync(server, attached.session, attached.accepted);

  const notes = notesOf(server.board());
  assert.equal(copiesOf(notes, " First."), 1, notes);
  assert.equal(copiesOf(notes, " Second."), 1, notes);
});

test("pending work a runtime starts on reaches the server once", async () => {
  const server = new FakeBoardServer(boardDocument());
  const typing = new AuthoringRuntime();
  const typed = openBoardSession(typing, server, resource);
  const accepted = server.snapshot().accepted_frontier_base64;
  await append(typed, " From the other runtime.");

  const attached = attachAccepted(new AuthoringRuntime(persisted(typing)), server);
  sync(server, typed, accepted);
  sync(server, attached.session, attached.accepted);

  const notes = notesOf(server.board());
  assert.equal(copiesOf(notes, " From the other runtime."), 1, notes);
});

/** A controller of a replica of `update` that takes no documents. */
function controllerWithoutDocuments(update: string): AuthoringSessionController<BoardDocument, BoardTextFieldPath> {
  const { replaceDraft: _takesDocuments, ...controller } = BOARD_DRAFTS.fromUpdate(update, peerBlock()).controller();
  return controller;
}

/** Attaches a replica of `update` whose controller takes no documents. */
function attachWithoutDocuments(runtime: AuthoringRuntime, update: string): AuthoringSessionHandle<BoardDocument, BoardTextFieldPath> {
  return runtime.ensureController(resource, () => controllerWithoutDocuments(update));
}

test("a replica that takes no documents restores a blocked session's pending work as its operations", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Held.");
  beforeRestart.block(resource, "recoveryRequired");

  const afterRestart = new AuthoringRuntime(persisted(beforeRestart));
  const restored = attachWithoutDocuments(afterRestart, server.snapshot().update_base64);
  assert.equal(copiesOf(notesOf(restored.currentDraft()), " Held."), 1);
  assert.equal(afterRestart.session(resource)?.status, "recoveryRequired");

  // What it restored stays recorded, so a further restart restores it again.
  const again = attachWithoutDocuments(new AuthoringRuntime(persisted(afterRestart)), server.snapshot().update_base64);
  assert.equal(copiesOf(notesOf(again.currentDraft()), " Held."), 1);
});

test("a replica that takes no documents holds pending work whose history it lacks", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Pending.");
  const pending = beforeRestart.session<BoardDocument>(resource)?.draft;
  assert.ok(pending);
  const otherHistory = BOARD_DRAFTS.fromDocument(boardDocument(), peerBlock()).exportUpdateBase64();

  const afterRestart = new AuthoringRuntime(persisted(beforeRestart));
  attachWithoutDocuments(afterRestart, otherHistory);
  const held = afterRestart.session<BoardDocument>(resource);
  assert.equal(held?.status, "recoveryRequired");
  assert.deepEqual(held?.draft, pending);

  // What it holds stays recorded, so a replica with that history restores it.
  const restored = attachWithoutDocuments(new AuthoringRuntime(persisted(afterRestart)), server.snapshot().update_base64);
  assert.equal(copiesOf(notesOf(restored.currentDraft()), " Pending."), 1);
});

test("held pending work keeps the peer blocks it was written under until a replica restores it", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Pending.");
  const written = beforeRestart.session(resource)?.draftOperations?.peerNonces ?? [];
  assert.ok(written.length > 0, "the draft records the blocks it was written under");
  const otherHistory = BOARD_DRAFTS.fromDocument(boardDocument(), peerBlock()).exportUpdateBase64();

  const afterRestart = new AuthoringRuntime(persisted(beforeRestart));
  attachWithoutDocuments(afterRestart, otherHistory);
  assert.equal(afterRestart.session(resource)?.status, "recoveryRequired");
  assert.deepEqual(afterRestart.session(resource)?.draftOperations?.peerNonces, written);

  const restoring = new AuthoringRuntime(persisted(afterRestart));
  attachWithoutDocuments(restoring, server.snapshot().update_base64);
  const held = restoring
    .controller<{ peerNonces(): readonly string[] }>(resource)
    ?.peerNonces() ?? [];
  for (const nonce of written) {
    assert.ok(held.includes(nonce), `the restoring replica names ${nonce}`);
  }
});

test("held pending work stays held and persisted across a disconnect and a reconnect", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Pending.");
  const otherHistory = BOARD_DRAFTS.fromDocument(boardDocument(), peerBlock()).exportUpdateBase64();

  const runtime = new AuthoringRuntime(persisted(beforeRestart));
  attachWithoutDocuments(runtime, otherHistory);
  const held = runtime.session<BoardDocument>(resource);
  runtime.disconnect();
  const offline = persisted(runtime).sessions[`${resource.entity}:${resource.resourceKey}`];
  assert.equal(offline?.status, "recoveryRequired");
  assert.deepEqual(offline?.draft, held?.draft);
  assert.deepEqual(offline?.draftOperations, held?.draftOperations);

  runtime.reconnect();
  assert.equal(runtime.session(resource)?.status, "recoveryRequired");
  assert.deepEqual(runtime.session(resource)?.draft, held?.draft);
});

test("a subscriber that attaches while pending work is held gets the one controller", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Pending.");
  const otherHistory = BOARD_DRAFTS.fromDocument(boardDocument(), peerBlock()).exportUpdateBase64();

  const runtime = new AuthoringRuntime(persisted(beforeRestart));
  let created = 0;
  const create = () => {
    created += 1;
    return controllerWithoutDocuments(otherHistory);
  };
  const attachedFromSubscribers: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>[] = [];
  runtime.subscribe(() => attachedFromSubscribers.push(runtime.ensureController(resource, create)));
  const handle = runtime.ensureController(resource, create);

  assert.equal(created, 1);
  assert.ok(attachedFromSubscribers.every((attached) => attached === handle));
});

test("a replica that takes no documents holds a draft that has no recorded operations", async () => {
  const server = new FakeBoardServer(boardDocument());
  const typing = new AuthoringRuntime();
  await append(openBoardSession(typing, server, resource), " Pending.");
  const detached = new AuthoringRuntime(persisted(typing));
  const replacement = withNotes(server.board(), "Rewritten.");
  detached.modify(resource, replacement);
  assert.equal(detached.session(resource)?.draftOperations, undefined);

  attachWithoutDocuments(detached, server.snapshot().update_base64);
  const held = detached.session<BoardDocument>(resource);
  assert.equal(held?.status, "recoveryRequired");
  assert.deepEqual(held?.draft, replacement);
  assert.equal(held?.draftOperations, undefined);
  assert.equal(persisted(detached).sessions[`${resource.entity}:${resource.resourceKey}`]?.draftOperations, undefined);
});

test("a controller holding no draft leaves a pending session editable", () => {
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource,
    policy: "optimisticDocument",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["optimisticDocument"],
    baseline: boardDocument(),
    draft: boardDocument()
  });
  runtime.modify(resource, withNotes(boardDocument(), "Edited."));
  runtime.ensureController(resource, () => ({ dispose: () => undefined }));

  const next = withNotes(boardDocument(), "Edited again.");
  runtime.modify(resource, next);
  assert.deepEqual(runtime.session(resource)?.draft, next);
});

test("discarding pending work a replica could not hold leaves the replica's document, and records what follows", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Pending.");
  const replicaDocument = withNotes(boardDocument(), "Elsewhere.");
  const replicaHistory = BOARD_DRAFTS.fromDocument(replicaDocument, peerBlock()).exportUpdateBase64();

  const afterRestart = new AuthoringRuntime(persisted(beforeRestart));
  const session = attachWithoutDocuments(afterRestart, replicaHistory);
  afterRestart.discard(resource);
  assert.equal(afterRestart.session(resource)?.draftOperations, undefined);
  assert.deepEqual(session.currentDraft(), replicaDocument);

  await append(session, " After.");
  const restored = attachWithoutDocuments(new AuthoringRuntime(persisted(afterRestart)), replicaHistory);
  const notes = notesOf(restored.currentDraft());
  assert.equal(copiesOf(notes, " After."), 1, notes);
  assert.equal(copiesOf(notes, " Pending."), 0, notes);
});

test("a replica holding other history takes the draft as a document", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Pending.");
  const snapshot = persisted(beforeRestart);
  const draft = beforeRestart.session<BoardDocument>(resource)?.draft;
  assert.ok(draft);

  const afterRestart = new AuthoringRuntime(snapshot);
  const session = afterRestart.ensureController(resource, () =>
    BOARD_DRAFTS.fromDocument(boardDocument(), peerBlock()).controller());

  assert.deepEqual(session.currentDraft(), draft);
});

test("a draft replaced without a replica attaches as the replacement", async () => {
  const server = new FakeBoardServer(boardDocument());
  const typing = new AuthoringRuntime();
  await append(openBoardSession(typing, server, resource), " Pending.");
  const restored = new AuthoringRuntime(persisted(typing));
  const replacement = withNotes(server.board(), "Rewritten.");

  restored.modify(resource, replacement);
  const { session } = attachAccepted(restored, server);

  assert.deepEqual(session.currentDraft(), replacement);
});

test("a draft discarded without a replica attaches as the accepted document", async () => {
  const server = new FakeBoardServer(boardDocument());
  const typing = new AuthoringRuntime();
  await append(openBoardSession(typing, server, resource), " Pending.");
  const restored = new AuthoringRuntime(persisted(typing));

  restored.discard(resource);
  const { session } = attachAccepted(restored, server);

  assert.deepEqual(session.currentDraft(), server.board());
});

test("a restart keeps edits accepted meanwhile when pending work follows a character typed and deleted", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  const typed = openBoardSession(beforeRestart, server, resource);
  await append(typed, "x");
  await trim(typed, 1);
  await append(typed, " Offline.");
  const snapshot = persisted(beforeRestart);
  server.editRemotely((board) => withNotes(board, `Remote. ${notesOf(board)}`));

  const { session } = attachAccepted(new AuthoringRuntime(snapshot), server);

  const notes = notesOf(session.currentDraft());
  assert.equal(copiesOf(notes, " Offline."), 1, notes);
  assert.equal(copiesOf(notes, "Remote. "), 1, notes);
});

test("a restart after a save takes what was pending since and keeps edits accepted meanwhile", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  const typed = openBoardSession(beforeRestart, server, resource);
  await append(typed, " Saved.");
  save(server, typed);
  await append(typed, " Pending.");
  const snapshot = persisted(beforeRestart);
  server.editRemotely((board) => withNotes(board, `Remote. ${notesOf(board)}`));

  const { session, accepted } = attachAccepted(new AuthoringRuntime(snapshot), server);
  sync(server, session, accepted);

  const merged = notesOf(server.board());
  for (const fragment of [" Saved.", " Pending.", "Remote. "]) {
    assert.equal(copiesOf(merged, fragment), 1, merged);
  }
});

test("another runtime's pending work is carried, so a replica behind it never writes it", async () => {
  const server = new FakeBoardServer(boardDocument());
  const older = server.snapshot();
  const typing = new AuthoringRuntime();
  const typed = openBoardSession(typing, server, resource);
  await append(typed, " First.");
  save(server, typed);
  await append(typed, " Pending.");
  const pending = toPersistedRuntime(typing.getSnapshot());

  const watching = new AuthoringRuntime();
  watching.restore(fromPersistedRuntime(pending));
  // As an application attaches a resource: open it unless a session exists.
  if (watching.session(resource) === undefined) {
    watching.open({
      resource,
      policy: "collaborative",
      schemaVersion: older.schema_version,
      acceptedRevision: older.accepted_frontier_base64,
      supportedExchangeModes: [...older.exchange_modes],
      baseline: boardDocument(),
      draft: boardDocument()
    });
  }
  const watched = watching.ensureController(resource, () =>
    BOARD_DRAFTS.fromUpdate(older.update_base64, peerBlock()).controller());
  sync(server, typed, typed.state().acceptedRevision);
  sync(server, watched, older.accepted_frontier_base64);

  const notes = notesOf(server.board());
  assert.equal(copiesOf(notes, " First."), 1, notes);
  assert.equal(copiesOf(notes, " Pending."), 1, notes);
  assert.deepEqual(toPersistedRuntime(watching.persistedState()), pending);
});

test("a restored draft keeps the peer blocks its operations were written under", async () => {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  const session = openBoardSession(runtime, server, resource);
  await append(session, " Pending.");
  const written = runtime.session(resource)?.draftOperations?.peerNonces ?? [];
  assert.ok(written.length > 0, "the draft records the blocks it was written under");

  const restored = new AuthoringRuntime(persisted(runtime));
  assert.deepEqual(restored.session(resource)?.draftOperations?.peerNonces, written);
  attachAccepted(restored, server);

  const held = restored
    .controller<{ peerNonces(): readonly string[] }>(resource)
    ?.peerNonces() ?? [];
  for (const nonce of written) {
    assert.ok(held.includes(nonce), `the restored replica names ${nonce}`);
  }
});
