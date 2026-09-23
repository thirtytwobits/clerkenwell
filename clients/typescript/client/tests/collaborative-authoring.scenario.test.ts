/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A collaborative authoring session over a live replica: saves export only
 * their edit, accepted state is imported before the retained draft is
 * reconciled, and typing made while a save is in flight survives it.
 */
import assert from "node:assert/strict";
import test from "node:test";

import { AuthoringRuntime } from "@clerkenwell/client";

import {
  addTask,
  FakeBoardServer,
  openBoardSession,
  relabelTask,
  renameTask
} from "./support/board-server";
import { boardDocument } from "./support/plans";

const resource = { entity: "Board", resourceKey: "board-1" } as const;

test("a session converges through saves, echoes and remote edits", () => {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  const session = openBoardSession(runtime, server, resource);
  const save = (): void => {
    const base = session.state().acceptedRevision;
    const accepted = server.accept({
      baseFrontierBase64: base,
      updateBase64: session.exportIncrementalUpdateBase64(base)
    });
    session.adoptAccepted({
      updateBase64: accepted.state.update_base64,
      baseline: accepted.board,
      acceptedRevision: accepted.state.accepted_frontier_base64
    });
  };

  session.replaceDraft(addTask(session.currentDraft(), "todo", "task-4"));
  save();
  session.replaceDraft(relabelTask(session.currentDraft(), "task-4", ["urgent"]));
  save();
  assert.equal(server.imports.length, 2);
  assert.ok(server.imports.every(
    ({ updateBase64 }) => updateBase64.length < server.snapshot().update_base64.length
  ));

  session.replaceDraft(renameTask(session.currentDraft(), "task-4", "Order badges"));
  const remote = server.editRemotely((board) => renameTask(board, "task-2", "Book a bigger venue"));
  session.adoptAccepted({
    updateBase64: remote.update_base64,
    baseline: server.board(),
    acceptedRevision: remote.accepted_frontier_base64
  });
  save();

  const titles = new Map(server.board().columns.flatMap(({ tasks }) => tasks).map(({ id, title }) => [id, title]));
  assert.equal(titles.get("task-4"), "Order badges");
  assert.equal(titles.get("task-2"), "Book a bigger venue");
  assert.deepEqual(session.currentDraft(), server.board());
  assert.equal(session.state().status, "clean");
});

test("a save acknowledgement keeps typing made while the save was in flight", () => {
  const server = new FakeBoardServer(boardDocument());
  const runtime = new AuthoringRuntime();
  const session = openBoardSession(runtime, server, resource);
  const baseRevision = session.state().acceptedRevision;
  const submitted = renameTask(session.currentDraft(), "task-1", "Submitted title");
  session.replaceDraft(submitted);
  const updateBase64 = session.exportIncrementalUpdateBase64(baseRevision);
  runtime.queue(resource, {
    operationId: "board-save",
    schemaVersion: server.snapshot().schema_version,
    exchangeMode: "incremental",
    baseRevision,
    updateBase64,
    document: submitted
  });
  runtime.beginReplay(resource, "board-save");
  const accepted = server.accept({ baseFrontierBase64: baseRevision, updateBase64 });

  const later = "Typed during the save";
  session.replaceDraft(renameTask(session.currentDraft(), "task-3", later));
  session.adoptAccepted({
    updateBase64: accepted.state.update_base64,
    baseline: accepted.board,
    acceptedRevision: accepted.state.accepted_frontier_base64
  });
  runtime.acknowledge({
    resource,
    operationId: "board-save",
    document: accepted.board,
    acceptedRevision: accepted.state.accepted_frontier_base64
  });

  const titles = new Map(
    session.state().draft.columns.flatMap(({ tasks }) => tasks).map(({ id, title }) => [id, title])
  );
  assert.equal(titles.get("task-1"), "Submitted title");
  assert.equal(titles.get("task-3"), later);
  assert.deepEqual(session.state().baseline, accepted.board);
  assert.equal(session.state().status, "modified");
});
