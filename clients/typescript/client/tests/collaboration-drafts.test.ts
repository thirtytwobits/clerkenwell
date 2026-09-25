/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Replicas read and written as an application's draft of their document.
 */
import assert from "node:assert/strict";
import test from "node:test";

import { AuthoringRuntime } from "@clerkenwell/client";
import { CollaborationDrafts, documentDraftMapping } from "@clerkenwell/client/loro";

import { BOARD_PLAN, boardDocument, type BoardColumn, type BoardDocument } from "./support/plans";
import { insert } from "./support/text";

/** Drafts a board as its columns alone. */
const COLUMN_DRAFTS = new CollaborationDrafts("Board", BOARD_PLAN, {
  toDraft: (board: BoardDocument) => board.columns,
  toDocument: (columns: BoardColumn[], board: BoardDocument) => ({ ...board, columns })
});

const BOARDS = new CollaborationDrafts("Board", BOARD_PLAN, documentDraftMapping<BoardDocument>());

function renamed(columns: BoardColumn[], name: string): BoardColumn[] {
  return columns.map((column, index) => (index === 0 ? { ...column, name } : column));
}

test("a draft is what the mapping reads from the replica's document", () => {
  const board = boardDocument();

  assert.deepEqual(COLUMN_DRAFTS.fromDocument(board).currentDraft(), board.columns);
});

test("a replaced draft is written into the document, which keeps what the draft does not hold", () => {
  const board = boardDocument();
  const replica = COLUMN_DRAFTS.fromDocument(board);
  const columns = renamed(board.columns, "Backlog");

  replica.replaceDraft(columns);

  const written = BOARDS.fromUpdate(replica.exportUpdateBase64()).currentDraft();
  assert.deepEqual(written.columns, columns);
  assert.equal(written.title, board.title);
  assert.equal(written.boardId, board.boardId);
});

test("a mapped draft is read and written through both mappings", () => {
  const board = boardDocument();
  const firstColumnName = COLUMN_DRAFTS.map({
    toDraft: (columns) => columns[0]?.name ?? "",
    toDocument: (name: string, columns) => renamed(columns, name)
  });
  const replica = firstColumnName.fromDocument(board);
  assert.equal(replica.currentDraft(), board.columns[0]?.name);

  replica.replaceDraft("Backlog");

  const written = BOARDS.fromUpdate(replica.exportUpdateBase64()).currentDraft();
  assert.deepEqual(written, { ...board, columns: renamed(board.columns, "Backlog") });
});

test("a draft whose document the plan refuses leaves the replica as it was", () => {
  const board = boardDocument();
  const replica = COLUMN_DRAFTS.fromDocument(board);
  const [first, ...rest] = board.columns;
  assert.ok(first);
  const unidentified = { ...first, columnId: undefined } as unknown as BoardColumn;

  assert.throws(() => replica.replaceDraft([unidentified, ...rest]));

  assert.deepEqual(replica.currentDraft(), board.columns);
});

test("text typed into a draft replica's field reaches its draft", () => {
  const replica = COLUMN_DRAFTS.fromDocument(boardDocument());
  const brief = replica.bindText("columns.*.brief", { column_id: "todo" });

  insert(brief, 0, "Urgent. ");

  assert.equal(replica.currentDraft()[0]?.brief, "Urgent. Not started.");
});

test("a fork's edits reach the replica it came from only once imported", () => {
  const board = boardDocument();
  const replica = COLUMN_DRAFTS.fromDocument(board);
  const frontier = replica.acceptedFrontierBase64();
  const fork = replica.fork();
  const columns = renamed(board.columns, "Backlog");

  fork.replaceDraft(columns);
  assert.deepEqual(replica.currentDraft(), board.columns);

  replica.importUpdateBase64(fork.exportIncrementalUpdateBase64(frontier));
  assert.deepEqual(replica.currentDraft(), columns);
});

test("drafts refuse accepted state written under another schema version", () => {
  const replica = COLUMN_DRAFTS.fromDocument(boardDocument());
  const other = BOARD_PLAN.schemaVersion + 1;

  assert.doesNotThrow(() => COLUMN_DRAFTS.requireSchemaVersion(BOARD_PLAN.schemaVersion));
  assert.throws(() => COLUMN_DRAFTS.requireSchemaVersion(other));
  assert.throws(() =>
    replica.importVersionedUpdateBase64(other, replica.exportUpdateBase64()));
});

test("an authoring runtime records a mapped replica's pending work as its operations", async () => {
  const board = boardDocument();
  const accepted = BOARDS.fromDocument(board);
  const resource = { entity: "Board", resourceKey: board.boardId } as const;
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: BOARD_PLAN.schemaVersion,
    acceptedRevision: accepted.acceptedFrontierBase64(),
    supportedExchangeModes: ["incremental"],
    baseline: board.columns,
    draft: board.columns
  });
  const session = runtime.ensureController(resource, () =>
    COLUMN_DRAFTS.fromUpdate(accepted.exportUpdateBase64()).controller());

  insert(session.bindText("columns.*.brief", { column_id: "done" }), 0, "Really. ");
  await new Promise<void>((resolve) => queueMicrotask(resolve));

  const pending = runtime.session<BoardColumn[]>(resource);
  assert.equal(pending?.draft[1]?.brief, "Really. Shipped.");
  assert.ok(pending?.draftOperations, "pending work is recorded as operations");
});
