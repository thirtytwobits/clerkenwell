/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * One runtime-owned session per resource, backed by a live replica.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  AuthoringRuntime,
  fromPersistedRuntime,
  toPersistedRuntime
} from "@clerkenwell/client";

import { FakeBoardServer, openBoardSession, renameTask } from "./support/board-server";
import { BOARD_PLAN, boardDocument, type BoardDocument } from "./support/plans";
import { insert } from "./support/text";

const resource = { entity: "Board", resourceKey: "board-1" } as const;

function opened() {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  const session = openBoardSession(runtime, server, resource);
  return { server, runtime, session };
}

test("a resource has one runtime-owned session whose draft starts at the baseline", () => {
  const { runtime, session } = opened();

  assert.equal(runtime.authoringSession<BoardDocument>(resource), session);
  assert.deepEqual(session.currentDraft(), session.state().baseline);
  assert.equal(session.state().status, "clean");
});

test("a save exports only its edit over the accepted frontier", () => {
  const { server, session } = opened();
  const accepted = session.state().acceptedRevision;
  session.replaceDraft(renameTask(session.currentDraft(), "task-2", "Book the hall"));

  const result = server.accept({
    baseFrontierBase64: accepted,
    updateBase64: session.exportIncrementalUpdateBase64(accepted)
  });
  session.adoptAccepted({
    updateBase64: result.state.update_base64,
    baseline: result.board,
    acceptedFrontierBase64: result.state.accepted_frontier_base64,
    acceptedRevision: result.state.accepted_frontier_base64
  });

  assert.deepEqual(session.currentDraft(), server.board());
  assert.deepEqual(session.state().baseline, server.board());
  assert.equal(session.state().status, "clean");
});

test("a remote update merges with a retained local draft", () => {
  const { server, session } = opened();
  const local = "Local title";
  const remoteTitle = "Remote title";
  session.replaceDraft(renameTask(session.currentDraft(), "task-1", local));
  const remote = server.editRemotely((board) => renameTask(board, "task-3", remoteTitle));

  session.adoptAccepted({
    updateBase64: remote.update_base64,
    baseline: server.board(),
    acceptedFrontierBase64: remote.accepted_frontier_base64,
    acceptedRevision: remote.accepted_frontier_base64
  });

  const titles = new Map(
    session.currentDraft().columns.flatMap(({ tasks }) => tasks).map(({ id, title }) => [id, title])
  );
  assert.equal(titles.get("task-1"), local);
  assert.equal(titles.get("task-3"), remoteTitle);
  assert.deepEqual(session.state().baseline, server.board());
  assert.equal(session.state().status, "modified");
});

test("a refused save leaves the session's draft exactly as it was", () => {
  const { server, session } = opened();
  const draft = renameTask(session.currentDraft(), "task-2", "Retained draft");

  session.replaceDraft(draft);

  assert.deepEqual(session.currentDraft(), draft);
  assert.deepEqual(session.state().baseline, server.board());
  assert.equal(session.state().status, "modified");
});

test("a session refuses accepted state from another schema version", () => {
  const { server, session } = opened();

  assert.throws(
    () => session.adoptAccepted({
      updateBase64: server.snapshot().update_base64,
      schemaVersion: BOARD_PLAN.schemaVersion + 1,
      baseline: server.board(),
      acceptedFrontierBase64: server.snapshot().accepted_frontier_base64,
      acceptedRevision: server.snapshot().accepted_frontier_base64
    }),
    /schema version/
  );
});

test("a text binding edits the session draft", async () => {
  const { runtime, session } = opened();
  const target = { column_id: "todo", task_id: "task-1" };
  const notes = session.bindText("columns.*.tasks.*.notes", target);
  const addition = " Then edit.";
  const at = notes.read().length;

  notes.edit({
    baseRevision: notes.revision,
    changes: [{ from: at, to: at, insert: addition }],
    selectionBefore: { anchor: at, head: at },
    selectionAfter: { anchor: at + addition.length, head: at + addition.length },
    group: "typing"
  });
  await new Promise<void>((resolve) => queueMicrotask(resolve));

  const draft = runtime.session<BoardDocument>(resource)?.draft;
  assert.equal(draft?.columns[0]?.tasks[0]?.notes, notes.read());
  assert.equal(runtime.session(resource)?.status, "modified");
});

test("restoring another runtime's snapshot authors nothing in a live replica", async () => {
  const server = new FakeBoardServer(boardDocument());
  const typing = new AuthoringRuntime();
  const watching = new AuthoringRuntime();
  const typed = openBoardSession(typing, server, resource);
  const watched = openBoardSession(watching, server, resource);
  const accepted = server.snapshot().accepted_frontier_base64;
  const notes = typed.bindText("columns.*.tasks.*.notes", { column_id: "todo", task_id: "task-1" });
  const addition = " Written once.";
  insert(notes, notes.read().length, addition);
  await new Promise<void>((resolve) => queueMicrotask(resolve));

  let published = 0;
  watching.subscribe(() => { published += 1; });
  const before = watched.state();
  watching.restore(fromPersistedRuntime(toPersistedRuntime(typing.getSnapshot())));

  assert.equal(published, 0, "An unchanged runtime has nothing to persist back");
  assert.equal(watched.state(), before);
  for (const session of [typed, watched]) {
    server.accept({
      baseFrontierBase64: accepted,
      updateBase64: session.exportIncrementalUpdateBase64(accepted)
    });
  }
  const merged = server.board().columns[0]?.tasks[0]?.notes ?? "";
  assert.equal(merged.split(addition).length - 1, 1, `Written once, merged as: ${merged}`);
});
