/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Concurrent edits judged field by field against each field's conflict policy.
 */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { conflictingFieldPaths } from "@clerkenwell/client";

import { COLLABORATION_PLANS } from "../../../../crates/clerkenwell-notebook/generated/typescript/index";

type Entity = keyof typeof COLLABORATION_PLANS;
type ClientRecord = Record<string, unknown>;

const corpus = JSON.parse(
  await readFile(
    new URL(
      "../../../../crates/clerkenwell-notebook/generated/notebook.collaboration.fixtures.json",
      import.meta.url
    ),
    "utf8"
  )
) as { entities: { entity: Entity; clientDocument: ClientRecord }[] };

function sample(entity: Entity): ClientRecord {
  const fixture = corpus.entities.find((candidate) => candidate.entity === entity);
  assert.ok(fixture, `a ${entity} fixture`);
  return structuredClone(fixture.clientDocument);
}

function judge(entity: Entity, client: ClientRecord, current: ClientRecord): string[] {
  return conflictingFieldPaths(COLLABORATION_PLANS[entity], sample(entity), client, current);
}

function withNote(changes: ClientRecord): ClientRecord {
  return { ...sample("Note"), ...changes };
}

/** The board with its first column's title replaced. */
function withFirstColumnTitle(title: string): ClientRecord {
  const board = sample("Board");
  const columns = board["columns"] as ClientRecord[];
  columns[0] = { ...columns[0], title };
  return board;
}

function firstColumnId(): string {
  const columns = sample("Board")["columns"] as ClientRecord[];
  const id = columns[0]?.["columnId"];
  assert.equal(typeof id, "string");
  return id as string;
}

test("an explicit field both sides changed to different values conflicts", () => {
  assert.deepEqual(
    judge("Note", withNote({ title: "Mine" }), withNote({ title: "Theirs" })),
    ["title"]
  );
});

test("an explicit field both sides changed to the same value does not conflict", () => {
  assert.deepEqual(judge("Note", withNote({ title: "Agreed" }), withNote({ title: "Agreed" })), []);
});

test("an explicit field only one side changed does not conflict", () => {
  assert.deepEqual(judge("Note", withNote({ title: "Mine" }), sample("Note")), []);
  assert.deepEqual(judge("Note", sample("Note"), withNote({ title: "Theirs" })), []);
});

test("an immutable field the edit changes conflicts even when nothing else changed it", () => {
  assert.deepEqual(judge("Note", withNote({ createdAt: "rewritten" }), sample("Note")), ["created_at"]);
});

test("merged and last-writer-wins fields never conflict", () => {
  assert.deepEqual(
    judge(
      "Note",
      withNote({ summary: "Mine", pinned: false }),
      withNote({ summary: "Theirs", pinned: true, weight: 9 })
    ),
    []
  );
});

test("an explicit field of a nested group is named by its wire path", () => {
  const meta = sample("Note")["meta"] as ClientRecord;
  assert.deepEqual(
    judge("Note", withNote({ meta: { ...meta, owner: "Mine" } }), withNote({ meta: { ...meta, owner: "Theirs" } })),
    ["meta.owner"]
  );
});

test("a keyed item's field is judged per item and named by the item's identity", () => {
  assert.deepEqual(
    judge("Board", withFirstColumnTitle("Mine"), withFirstColumnTitle("Theirs")),
    [`columns[${firstColumnId()}].title`]
  );
});
