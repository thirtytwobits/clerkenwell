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

import { FakeBoardServer, openBoardSession } from "./support/board-server";
import { boardDocument, type BoardDocument } from "./support/plans";
import { BOARD_DRAFTS, type BoardTextFieldPath } from "./support/replicas";
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
    baseline: result.board,
    acceptedRevision: result.state.accepted_frontier_base64
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
      BOARD_DRAFTS.fromUpdate(state.update_base64).controller()),
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

test("a replica holding other history takes the draft as a document", async () => {
  const server = new FakeBoardServer(boardDocument());
  const beforeRestart = new AuthoringRuntime();
  await append(openBoardSession(beforeRestart, server, resource), " Pending.");
  const snapshot = persisted(beforeRestart);
  const draft = beforeRestart.session<BoardDocument>(resource)?.draft;
  assert.ok(draft);

  const afterRestart = new AuthoringRuntime(snapshot);
  const session = afterRestart.ensureController(resource, () =>
    BOARD_DRAFTS.fromDocument(boardDocument()).controller());

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
    BOARD_DRAFTS.fromUpdate(older.update_base64).controller());
  sync(server, typed, typed.state().acceptedRevision);
  sync(server, watched, older.accepted_frontier_base64);

  const notes = notesOf(server.board());
  assert.equal(copiesOf(notes, " First."), 1, notes);
  assert.equal(copiesOf(notes, " Pending."), 1, notes);
  assert.deepEqual(toPersistedRuntime(watching.persistedState()), pending);
});
