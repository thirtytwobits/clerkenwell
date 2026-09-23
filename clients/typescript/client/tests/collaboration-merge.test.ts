/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Whole-document writes are diffs, so concurrent writers merge per storage
 * kind instead of replacing each other, and converge in either import order.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  collaborationDocumentToLoroDoc,
  materializeCollaborationDocumentFromLoroDoc
} from "@clerkenwell/client/loro";

import {
  BOARD_PLAN,
  boardDocument,
  noteDocument,
  type BoardDocument,
  type NoteDocument
} from "./support/plans";
import { boardReplica, mergeRewrites, noteReplica } from "./support/replicas";

function mergeNotes(
  base: NoteDocument,
  left: (note: NoteDocument) => NoteDocument,
  right: (note: NoteDocument) => NoteDocument
): NoteDocument {
  const { forward, reverse } = mergeRewrites(noteReplica(base), [left, right]);
  assert.deepEqual(forward, reverse);
  return forward;
}

function mergeBoards(
  base: BoardDocument,
  left: (board: BoardDocument) => BoardDocument,
  right: (board: BoardDocument) => BoardDocument
): BoardDocument {
  const { forward, reverse } = mergeRewrites(boardReplica(base), [left, right]);
  assert.deepEqual(forward, reverse);
  return forward;
}

test("edits to different scalars both survive", () => {
  const title = "Revised minutes";
  const priority = 9;

  const merged = mergeNotes(
    noteDocument(),
    (note) => ({ ...note, title }),
    (note) => ({ ...note, priority })
  );

  assert.equal(merged.title, title);
  assert.equal(merged.priority, priority);
});

test("concurrent edits to one text field merge instead of replacing", () => {
  const base = { ...noteDocument(), body: "Chapter" };
  const prefix = "A ";
  const suffix = " One";

  const merged = mergeNotes(
    base,
    (note) => ({ ...note, body: note.body + suffix }),
    (note) => ({ ...note, body: prefix + note.body })
  );

  assert.equal(merged.body, prefix + base.body + suffix);
});

test("concurrent list additions keep both and the shared entries once", () => {
  const base = noteDocument();

  const merged = mergeNotes(
    base,
    (note) => ({ ...note, tags: [...note.tags, "appended"] }),
    (note) => ({ ...note, tags: ["prepended", ...note.tags] })
  );

  assert.deepEqual(merged.tags, ["prepended", ...base.tags, "appended"]);
});

test("a structured entry rewritten on one side survives an entry added on the other", () => {
  const base = noteDocument();
  const rewritten = { targetId: base.links[0]!.targetId, linkLabel: "Rewritten" };
  const added = { targetId: "note-2", linkLabel: "Next" };

  const merged = mergeNotes(
    base,
    (note) => ({ ...note, links: [rewritten] }),
    (note) => ({ ...note, links: [...note.links, added] })
  );

  assert.deepEqual(merged.links, [rewritten, added]);
});

test("disjoint keys added to a structured map both survive", () => {
  const base = noteDocument();

  const merged = mergeNotes(
    base,
    (note) => ({ ...note, attributes: { ...note.attributes, left: { writer: "left" } } }),
    (note) => ({ ...note, attributes: { ...note.attributes, right: { writer: "right" } } })
  );

  assert.deepEqual(merged.attributes, {
    ...base.attributes,
    left: { writer: "left" },
    right: { writer: "right" }
  });
});

test("structured document prose merges and identity-keyed entries added on both sides survive", () => {
  const heading = "Agenda";
  const prefix = "The ";
  const suffix = " for Friday";
  const shared = { id: "one", text: "One" };
  const base = { ...noteDocument(), outline: { heading, items: [shared] } };

  const merged = mergeNotes(
    base,
    (note) => ({
      ...note,
      outline: { heading: heading + suffix, items: [shared, { id: "left", text: "From the left" }] }
    }),
    (note) => ({
      ...note,
      outline: { heading: prefix + heading, items: [shared, { id: "right", text: "From the right" }] }
    })
  );

  const outline = merged.outline as { heading: string; items: { id: string; text: string }[] };
  assert.equal(outline.heading, prefix + heading + suffix);
  assert.deepEqual(new Set(outline.items.map(({ id }) => id)), new Set(["one", "left", "right"]));
  assert.equal(outline.items.find(({ id }) => id === "left")?.text, "From the left");
});

test("keyed items and nested keyed items added on both sides all survive", () => {
  const base = boardDocument();
  const leftTask = { id: "left-task", title: "Left", notes: "", labels: [] };
  const rightTask = { id: "right-task", title: "Right", notes: "", labels: [] };
  const leftColumn = { columnId: "left-column", name: "Left", brief: "", tasks: [] };
  const rightColumn = { columnId: "right-column", name: "Right", brief: "", tasks: [] };

  const merged = mergeBoards(
    base,
    (board) => {
      const next = structuredClone(board);
      next.columns[0]!.tasks.push(leftTask);
      next.columns.push(leftColumn);
      return next;
    },
    (board) => {
      const next = structuredClone(board);
      next.columns[0]!.tasks.push(rightTask);
      next.columns.push(rightColumn);
      return next;
    }
  );

  assert.deepEqual(
    new Set(merged.columns.map(({ columnId }) => columnId)),
    new Set([...base.columns.map(({ columnId }) => columnId), leftColumn.columnId, rightColumn.columnId])
  );
  assert.deepEqual(
    new Set(merged.columns[0]!.tasks.map(({ id }) => id)),
    new Set([...base.columns[0]!.tasks.map(({ id }) => id), leftTask.id, rightTask.id])
  );
});

test("an item edited on one side survives its reordering on the other", () => {
  const base = boardDocument();
  const renamed = "Renamed while moved";

  const merged = mergeBoards(
    base,
    (board) => {
      const next = structuredClone(board);
      next.columns[0]!.tasks[0]!.title = renamed;
      return next;
    },
    (board) => ({ ...board, columns: [...board.columns].reverse() })
  );

  assert.deepEqual(
    merged.columns.map(({ columnId }) => columnId),
    [...base.columns].reverse().map(({ columnId }) => columnId)
  );
  const column = merged.columns.find(({ columnId }) => columnId === base.columns[0]!.columnId);
  assert.equal(column?.tasks[0]?.title, renamed);
});

test("independently seeded items with the same identity materialise once", () => {
  const base = boardDocument();
  const left = collaborationDocumentToLoroDoc(BOARD_PLAN, {
    ...base,
    columns: [{ ...base.columns[0]!, name: "Left name" }]
  });
  const right = collaborationDocumentToLoroDoc(BOARD_PLAN, {
    ...base,
    columns: [{ ...base.columns[0]!, brief: "Right brief" }]
  });
  const replica = collaborationDocumentToLoroDoc(BOARD_PLAN, { ...base, columns: [] });
  replica.import(left.export({ mode: "update" }));
  replica.import(right.export({ mode: "update" }));

  const materialized = materializeCollaborationDocumentFromLoroDoc<BoardDocument>(replica, BOARD_PLAN, null);

  assert.deepEqual(materialized.columns.map(({ columnId }) => columnId), [base.columns[0]!.columnId]);
});
