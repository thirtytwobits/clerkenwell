/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A replica takes a persisted collaborative draft as the operations that made
 * it, so restoring pending work never authors those edits again.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  AuthoringRuntime,
  fromPersistedRuntime,
  toPersistedRuntime,
  type AuthoringRuntimeState,
  type AuthoringSessionHandle
} from "@clerkenwell/client";

import {
  FakeBoardServer,
  boardAuthoringController,
  openBoardSession
} from "./support/board-server";
import { boardDocument, type BoardDocument } from "./support/plans";
import { boardReplica, boardReplicaFromUpdate } from "./support/replicas";
import { insert } from "./support/text";

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

async function append(session: AuthoringSessionHandle<BoardDocument>, text: string): Promise<void> {
  const notes = session.bindText("columns.*.tasks.*.notes", noted);
  insert(notes, notes.read().length, text);
  await new Promise<void>((resolve) => queueMicrotask(resolve));
}

function persisted(runtime: AuthoringRuntime): AuthoringRuntimeState {
  return fromPersistedRuntime(toPersistedRuntime(runtime.getSnapshot()));
}

/** Attaches a replica built from what the server has accepted, as a reload does. */
function attachAccepted(runtime: AuthoringRuntime, server: FakeBoardServer): {
  session: AuthoringSessionHandle<BoardDocument>;
  accepted: string;
} {
  const state = server.snapshot();
  return {
    session: runtime.ensureController(resource, () =>
      boardAuthoringController(boardReplicaFromUpdate(state.update_base64))),
    accepted: state.accepted_frontier_base64
  };
}

/** Sends what a session's replica holds beyond an accepted frontier. */
function sync(
  server: FakeBoardServer,
  session: AuthoringSessionHandle<BoardDocument>,
  accepted: string
): void {
  server.accept({
    baseFrontierBase64: accepted,
    updateBase64: session.exportIncrementalUpdateBase64(accepted)
  });
}

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

test("a replica attaching to an earlier snapshot keeps edits accepted since it was taken", async () => {
  const server = new FakeBoardServer(boardDocument());
  const typing = new AuthoringRuntime();
  const typed = openBoardSession(typing, server, resource);
  const accepted = server.snapshot().accepted_frontier_base64;
  await append(typed, " First.");
  const earlier = persisted(typing);
  await append(typed, " Second.");
  sync(server, typed, accepted);

  const watching = new AuthoringRuntime();
  watching.restore(earlier);
  const attached = attachAccepted(watching, server);
  sync(server, attached.session, attached.accepted);

  const notes = notesOf(server.board());
  assert.equal(copiesOf(notes, " First."), 1, notes);
  assert.equal(copiesOf(notes, " Second."), 1, notes);
});

test("pending work adopted from another runtime reaches the server once", async () => {
  const server = new FakeBoardServer(boardDocument());
  const typing = new AuthoringRuntime();
  const typed = openBoardSession(typing, server, resource);
  const accepted = server.snapshot().accepted_frontier_base64;
  await append(typed, " From the other runtime.");

  const watching = new AuthoringRuntime();
  watching.restore(persisted(typing));
  const attached = attachAccepted(watching, server);
  sync(server, typed, accepted);
  sync(server, attached.session, attached.accepted);

  const notes = notesOf(server.board());
  assert.equal(copiesOf(notes, " From the other runtime."), 1, notes);
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
    boardAuthoringController(boardReplica(boardDocument())));

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
