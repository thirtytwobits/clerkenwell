/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A replica holds a collaborative document as its schema spells it. Its type
 * is the entity's generated document type with the members a replica may
 * leave out optional.
 */
import assert from "node:assert/strict";
import test from "node:test";

import type { CollaborationReplicaDocument } from "@clerkenwell/client";

import type * as Notebook from "../../../../crates/clerkenwell-notebook/generated/typescript/index.js";

/** `true` exactly when `A` and `B` are one type. */
type Same<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;

/** `true` exactly when every `A` is a `B`. */
type Assignable<A, B> = [A] extends [B] ? true : false;

interface Wire {
  doc_id: string;
  etag: string;
  title: string;
  meta: { owner_name: string; note?: string };
  columns: {
    column_id: string;
    revision: string;
    wip_limit?: number;
    cards: { card_id: string; fileName: string }[];
  }[];
  archive?: { revision: string; title: string }[];
}

test("a replica document naming no path is its document type", () => {
  const identities: readonly true[] = [true satisfies Same<CollaborationReplicaDocument<Wire>, Wire>];
  assert.ok(identities.every(Boolean));
});

test("a replica document makes optional exactly the members its paths name, at any depth", () => {
  type Held = CollaborationReplicaDocument<
    Wire,
    "doc_id" | "etag" | "meta.owner_name" | "columns.*.revision" | "archive.*.revision"
  >;
  const identities: readonly true[] = [
    true satisfies Same<Held, {
      doc_id?: string;
      etag?: string;
      title: string;
      meta: { owner_name?: string; note?: string };
      columns: {
        column_id: string;
        revision?: string;
        wip_limit?: number;
        cards: { card_id: string; fileName: string }[];
      }[];
      archive?: { revision?: string; title: string }[];
    }>
  ];
  assert.ok(identities.every(Boolean));
});

test("each collaborative entity's replica document has its generated document type's members", () => {
  type Note = Notebook.CollaborationDocument<"Note">;
  type Board = Notebook.CollaborationDocument<"Board">;
  type Column = Board["columns"][number];
  const identities: readonly true[] = [
    true satisfies Same<keyof Note, keyof Notebook.NoteDocument>,
    true satisfies Same<Note["attachments"], Notebook.Attachment[]>,
    true satisfies Same<keyof Board, keyof Notebook.BoardDocument>,
    true satisfies Same<keyof Column, keyof Notebook.BoardColumn>,
    true satisfies Same<keyof Column["cards"][number], keyof Notebook.BoardCard>
  ];
  assert.ok(identities.every(Boolean));
});

test("a document a replica holds need not carry the members it derives", () => {
  const holds: readonly true[] = [
    true satisfies Assignable<Notebook.NoteDocument, Notebook.CollaborationDocument<"Note">>,
    true satisfies Assignable<
      Omit<Notebook.NoteDocument, "note_id" | "etag">,
      Notebook.CollaborationDocument<"Note">
    >,
    true satisfies Assignable<
      Omit<Notebook.BoardDocument, "board_id" | "etag">,
      Notebook.CollaborationDocument<"Board">
    >
  ];
  assert.ok(holds.every(Boolean));
});
