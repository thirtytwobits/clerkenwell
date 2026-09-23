/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A document is validated whole before anything is written. A required field
 * must be present and satisfy its codec; an optional field that is absent or
 * null is removed, and a present one must satisfy its codec. A refused
 * document changes neither the replica's Loro document nor its mirror.
 */
import assert from "node:assert/strict";
import test from "node:test";

import type { CollaborationEntityPlan, CollaborationFieldPlan } from "@clerkenwell/client";
import {
  collaborationDocumentToLoroDoc,
  CollaborationLoroAuthoringDocument
} from "@clerkenwell/client/loro";

import {
  BOARD_PLAN,
  boardDocument,
  clientSegment,
  NOTE_PLAN,
  noteDocument,
  type BoardDocument,
  type NoteDocument
} from "./support/plans";

const REMOVE = Symbol("remove");

/** A copy of `document` with `value` at a dotted wire path; `*` applies to every item. */
function withValueAt<TDocument>(document: TDocument, wirePath: string, value: unknown): TDocument {
  const copy = structuredClone(document);
  const visit = (node: unknown, segments: readonly string[]): void => {
    const [head, ...rest] = segments;
    if (head === undefined || node === null || typeof node !== "object") {
      return;
    }
    if (head === "*") {
      if (Array.isArray(node)) node.forEach((item) => visit(item, rest));
      return;
    }
    const record = node as Record<string, unknown>;
    const key = clientSegment(head);
    if (rest.length > 0) {
      visit(record[key], rest);
    } else if (value === REMOVE) {
      delete record[key];
    } else {
      record[key] = value;
    }
  };
  visit(copy, wirePath.split("."));
  return copy;
}

/** Every value at a dotted wire path, following `*` into every item. */
function valuesAt(document: unknown, wirePath: string): unknown[] {
  const visit = (node: unknown, segments: readonly string[]): unknown[] => {
    const [head, ...rest] = segments;
    if (head === undefined) return [node];
    if (node === null || typeof node !== "object") return [];
    if (head === "*") return Array.isArray(node) ? node.flatMap((item) => visit(item, rest)) : [];
    const record = node as Record<string, unknown>;
    const key = clientSegment(head);
    return key in record ? visit(record[key], rest) : [];
  };
  return visit(document, wirePath.split("."));
}

function writtenFields(plan: CollaborationEntityPlan): CollaborationFieldPlan[] {
  return Object.values(plan.fields).filter(({ storage }) =>
    storage.kind !== "derivedIdentity" && storage.kind !== "derivedRevision");
}

function isEmptyForm(value: unknown): boolean {
  return value === ""
    || (Array.isArray(value) && value.length === 0)
    || (value !== null && typeof value === "object" && Object.keys(value).length === 0);
}

interface Case<TDocument extends object> {
  readonly name: string;
  readonly plan: CollaborationEntityPlan;
  readonly document: () => TDocument;
}

function boardWithArchive(): BoardDocument {
  return { ...boardDocument(), archive: [{ id: "retired", title: "Retired column" }] };
}

const NOTE: Case<NoteDocument> = { name: "Note", plan: NOTE_PLAN, document: noteDocument };
const BOARD: Case<BoardDocument> = { name: "Board", plan: BOARD_PLAN, document: boardWithArchive };
const CASES: readonly Case<object>[] = [NOTE, BOARD];

function replicaOf<TDocument extends object>(
  { name, plan }: Case<TDocument>,
  document: TDocument
): CollaborationLoroAuthoringDocument<TDocument> {
  return new CollaborationLoroAuthoringDocument<TDocument>(name, plan, { kind: "document", document });
}

function reread<TDocument extends object>(
  entry: Case<TDocument>,
  replica: CollaborationLoroAuthoringDocument<TDocument>
): TDocument {
  return new CollaborationLoroAuthoringDocument<TDocument>(entry.name, entry.plan, {
    kind: "update",
    updateBase64: replica.exportUpdateBase64()
  }).currentDocument();
}

/** The candidate is refused everywhere a document is written, and the replica is left as it was. */
function assertRefused<TDocument extends object>(
  entry: Case<TDocument>,
  candidate: TDocument,
  message: string
): void {
  assert.throws(() => replicaOf(entry, candidate), Error, `${message}: seeding a replica`);
  assert.throws(
    () => collaborationDocumentToLoroDoc(entry.plan, candidate),
    Error,
    `${message}: seeding a Loro document`
  );
  const replica = replicaOf(entry, entry.document());
  const update = replica.exportUpdateBase64();
  const frontier = replica.acceptedFrontierBase64();
  const mirror = replica.currentDocument();

  assert.throws(() => replica.replaceDocument(candidate), Error, `${message}: replacing a document`);

  assert.deepEqual(replica.currentDocument(), mirror, `${message}: the mirror changed`);
  assert.equal(replica.exportUpdateBase64(), update, `${message}: the Loro document changed`);
  assert.equal(replica.acceptedFrontierBase64(), frontier, `${message}: the frontier moved`);
}

test("a required field must be present", () => {
  for (const entry of CASES) {
    for (const field of writtenFields(entry.plan).filter(({ required }) => required)) {
      assertRefused(entry, withValueAt(entry.document(), field.path, REMOVE), `${entry.name}.${field.path} absent`);
      assertRefused(entry, withValueAt(entry.document(), field.path, null), `${entry.name}.${field.path} null`);
    }
  }
});

test("a required field must satisfy its codec", () => {
  const invalid: readonly (readonly [Case<object>, string, unknown])[] = [
    [NOTE, "title", 42],
    [NOTE, "priority", 2.5],
    [NOTE, "weight", "heavy"],
    [NOTE, "pinned", 1],
    [NOTE, "style.font_family", ["Serif"]],
    [NOTE, "body", { text: "body" }],
    [NOTE, "abstract", { "$mime": "image/png", value: "" }],
    [NOTE, "tags", ["kept", 7]],
    [NOTE, "links", [{ targetId: "a" }, "b"]],
    [BOARD, "columns", { todo: [] }],
    [BOARD, "columns.*.name", false],
    [BOARD, "columns.*.tasks.*.notes", 3],
    [BOARD, "columns.*.tasks.*.labels", "urgent"],
    [BOARD, "archive.*.title", 9]
  ];
  for (const [entry, path, value] of invalid) {
    assertRefused(entry, withValueAt(entry.document(), path, value), `${entry.name}.${path} = ${JSON.stringify(value)}`);
  }
});

test("a present optional value must satisfy its codec and is never coerced", () => {
  const invalid: readonly (readonly [Case<object>, string, unknown])[] = [
    [NOTE, "rating", "four"],
    [NOTE, "subtitle", 7],
    [NOTE, "style.accent_colour", false],
    [NOTE, "summary", 5],
    [NOTE, "attributes", "owner"],
    [NOTE, "attributes", ["owner"]],
    [NOTE, "outline", "Agenda"],
    [NOTE, "author", 3],
    [NOTE, "footnote", { text: "note" }],
    [NOTE, "caption", { "$mime": "application/json", value: "{}" }],
    [NOTE, "caption", "bare text"],
    [NOTE, "aliases", "one alias"],
    [NOTE, "aliases", ["kept", 2]],
    [NOTE, "references", ["note-9"]],
    [BOARD, "description", 3],
    [BOARD, "columns.*.tasks.*.estimate_hours", "3"],
    [BOARD, "archive", "retired"],
    [BOARD, "archive", [{ title: "No identity" }]]
  ];
  for (const [entry, path, value] of invalid) {
    assertRefused(entry, withValueAt(entry.document(), path, value), `${entry.name}.${path} = ${JSON.stringify(value)}`);
  }
});

test("an optional field that is absent or null is removed", () => {
  for (const entry of CASES) {
    for (const field of writtenFields(entry.plan).filter(({ required }) => !required)) {
      for (const removal of [REMOVE, null]) {
        const label = `${entry.name}.${field.path} ${removal === null ? "null" : "absent"}`;
        const replica = replicaOf(entry, entry.document());
        assert.ok(valuesAt(replica.currentDocument(), field.path).length > 0, `${label}: the fixture holds no value`);

        replica.replaceDocument(withValueAt(entry.document(), field.path, removal));

        for (const document of [replica.currentDocument(), reread(entry, replica)]) {
          for (const value of valuesAt(document, field.path)) {
            assert.ok(
              value === undefined || isEmptyForm(value),
              `${label}: came back as ${JSON.stringify(value)}`
            );
          }
        }
        const seeded = replicaOf(entry, withValueAt(entry.document(), field.path, removal));
        assert.deepEqual(reread(entry, seeded), reread(entry, replica), `${label}: seeding and removing differ`);
      }
    }
  }
});

test("removing an optional field that is already absent writes nothing", () => {
  for (const entry of CASES) {
    for (const field of writtenFields(entry.plan).filter(({ required }) => !required)) {
      const absent = withValueAt(entry.document(), field.path, REMOVE);
      const replica = replicaOf(entry, absent);
      const base = replica.acceptedFrontierBase64();
      const peer = new CollaborationLoroAuthoringDocument<object>(entry.name, entry.plan, {
        kind: "update",
        updateBase64: replica.exportUpdateBase64()
      });

      replica.replaceDocument(withValueAt(absent, field.path, null));

      assert.equal(
        peer.importUpdateBase64(replica.exportIncrementalUpdateBase64(base)),
        false,
        `${entry.name}.${field.path}`
      );
    }
  }
});

test("an empty optional string is absence", () => {
  const entry = NOTE;
  for (const path of ["subtitle", "summary"]) {
    const replica = replicaOf(entry, withValueAt(entry.document(), path, ""));

    assert.deepEqual(valuesAt(reread(entry, replica), path), [], path);
  }
});

test("a refused document leaves bound text and later writes working", () => {
  const entry = NOTE;
  const replica = replicaOf(entry, entry.document());
  const body = replica.bindText("body");
  const original = body.read();

  assert.throws(() => replica.replaceDocument(withValueAt(entry.document(), "rating", "four")));
  const title = "Accepted after a refusal";
  replica.replaceDocument(withValueAt(entry.document(), "title", title));

  assert.equal(body.read(), original);
  assert.equal(reread(entry, replica).title, title);
});
