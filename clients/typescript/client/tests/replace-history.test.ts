/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A replica never takes a history built apart from its own. When the store's
 * history is not the one a session's replica holds, as after a re-seed, the
 * session's replica is replaced: it takes the store's document, keeps unsent
 * edits made from that document, and holds any others for recovery.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  AuthoringRuntime,
  fromPersistedRuntime,
  toPersistedRuntime,
  type AuthoringRuntimeState
} from "@clerkenwell/client";
import { CollaborationUnrelatedHistoryError } from "@clerkenwell/client/replica";
import type { AuthoringSessionHandle } from "../src/authoring-runtime.js";

import { FakeBoardServer, openBoardSession, renameTask } from "./support/board-server.js";
import { peerBlock } from "./support/peers.js";
import { boardDocument, type BoardDocument } from "./support/plans.js";
import { BOARD_DRAFTS, boardReplica, type BoardTextFieldPath } from "./support/replicas.js";
import { insert } from "./support/text.js";

const resource = { entity: "Board", resourceKey: "board-1" } as const;
const noted = { column_id: "todo", task_id: "task-1" } as const;

function notesOf(board: BoardDocument): string {
  return board.columns[0]?.tasks[0]?.notes ?? "";
}

function copiesOf(text: string, fragment: string): number {
  return text.split(fragment).length - 1;
}

async function append(session: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>, text: string): Promise<void> {
  const notes = session.bindText("columns.*.tasks.*.notes", noted);
  insert(notes, notes.read().length, text);
  await new Promise<void>((resolve) => queueMicrotask(resolve));
}

function persisted(runtime: AuthoringRuntime): AuthoringRuntimeState {
  return fromPersistedRuntime(toPersistedRuntime(runtime.getSnapshot()));
}

/** Replaces the session's replica with one of what the server holds now. */
function replaceWithServers(
  runtime: AuthoringRuntime,
  server: FakeBoardServer
): AuthoringSessionHandle<BoardDocument, BoardTextFieldPath> {
  const state = server.snapshot();
  return runtime.replaceHistory(resource, {
    create: () => BOARD_DRAFTS.fromUpdate(state.update_base64, peerBlock()).controller(),
    acceptedRevision: state.accepted_frontier_base64
  });
}

/** Sends what the session's replica holds beyond the accepted revision. */
function send(server: FakeBoardServer, session: AuthoringSessionHandle<BoardDocument, BoardTextFieldPath>): void {
  const base = session.state().acceptedRevision;
  server.accept({ baseFrontierBase64: base, updateBase64: session.exportIncrementalUpdateBase64(base) });
}

test("a replica takes none of an update holding a history built apart from its own", () => {
  const replica = boardReplica(boardDocument());
  const before = replica.exportUpdateBase64();
  const separate = boardReplica(boardDocument());
  separate.replaceDocument(renameTask(separate.currentDocument(), "task-1", "Elsewhere"));

  assert.throws(
    () => replica.importUpdateBase64(separate.exportUpdateBase64()),
    CollaborationUnrelatedHistoryError
  );
  assert.equal(replica.exportUpdateBase64(), before);
});

test("a session with nothing pending takes a re-seeded document, each text once", () => {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  openBoardSession(runtime, server, resource);
  server.reseed(renameTask(boardDocument(), "task-1", "Re-seeded"));

  const session = replaceWithServers(runtime, server);

  assert.equal(runtime.session(resource)?.status, "clean");
  assert.deepEqual(session.currentDraft(), server.board());
  assert.equal(copiesOf(notesOf(session.currentDraft()), notesOf(boardDocument())), 1);
});

test("unsent edits made from the re-seeded document are kept and sent without the earlier history", async () => {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  await append(openBoardSession(runtime, server, resource), " Pending.");
  server.reseed(boardDocument());

  const session = replaceWithServers(runtime, server);
  assert.equal(runtime.session(resource)?.status, "modified");
  assert.equal(copiesOf(notesOf(session.currentDraft()), " Pending."), 1);

  send(server, session);
  const notes = notesOf(server.board());
  assert.equal(copiesOf(notes, " Pending."), 1, notes);
  assert.equal(copiesOf(notes, notesOf(boardDocument())), 1, notes);
});

test("a persisted session of another history keeps its unsent edits and never sends that history", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Pending.");
  server.reseed(boardDocument());

  const afterRestart = new AuthoringRuntime(persisted(beforeRestart));
  const session = replaceWithServers(afterRestart, server);
  assert.notEqual(afterRestart.session(resource)?.draftOperations?.peerNonces, undefined);

  send(server, session);
  const notes = notesOf(server.board());
  assert.equal(copiesOf(notes, " Pending."), 1, notes);
  assert.equal(copiesOf(notes, notesOf(boardDocument())), 1, notes);
});

test("unsent edits made from another document are held for recovery over the store's document", async () => {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  await append(openBoardSession(runtime, server, resource), " Pending.");
  const pending = runtime.session<BoardDocument>(resource)?.draft;
  server.reseed(renameTask(boardDocument(), "task-1", "Re-seeded"));

  replaceWithServers(runtime, server);

  const held = runtime.session<BoardDocument>(resource);
  assert.equal(held?.status, "recoveryRequired");
  assert.deepEqual(held?.draft, pending);
  assert.deepEqual(held?.baseline, server.board());
  runtime.discard(resource);
  assert.deepEqual(runtime.session(resource)?.draft, server.board());
});

test("a refusal requiring a resynchronisation blocks the session until its history is replaced", () => {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  const session = openBoardSession(runtime, server, resource);
  session.replaceDraft(renameTask(session.currentDraft(), "task-1", "Renamed"));
  runtime.queue(resource, {
    operationId: "rename",
    schemaVersion: 1,
    exchangeMode: "incremental",
    baseRevision: session.state().acceptedRevision,
    updateBase64: session.exportIncrementalUpdateBase64(session.state().acceptedRevision)
  });

  runtime.reject({
    resource,
    operationId: "rename",
    category: "collaboration_resync_required",
    message: "Resynchronise.",
    retryable: true
  });
  assert.equal(runtime.session(resource)?.status, "resyncRequired");
  assert.equal(runtime.session(resource)?.queuedOperations[0]?.state, "blocked");

  server.reseed(boardDocument());
  const replaced = replaceWithServers(runtime, server);

  assert.equal(runtime.session(resource)?.status, "modified");
  assert.deepEqual(runtime.session(resource)?.queuedOperations, []);
  assert.equal(replaced.currentDraft().columns[0]?.tasks[0]?.title, "Renamed");
});
