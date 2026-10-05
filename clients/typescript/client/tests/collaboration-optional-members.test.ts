/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A field its parent requires is present whenever the parent is; any other
 * optional field is present only when written.
 */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { peerBlock } from "./support/peers.js";

import { CollaborationReplica } from "@clerkenwell/client/replica";

import { COLLABORATION_PLANS } from "../../../../crates/clerkenwell-notebook/generated/typescript/index.js";

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

function roundTrip(entity: Entity, document: ClientRecord): ClientRecord {
  const plan = COLLABORATION_PLANS[entity];
  const written = CollaborationReplica.from(entity, plan, { kind: "document", document }, peerBlock());
  return CollaborationReplica.from<ClientRecord>(entity, plan, {
    kind: "update",
    updateBase64: written.exportUpdateBase64()
  }, peerBlock()).currentDocument();
}

/** A board whose one archived column holds `column`. */
function archived(column: ClientRecord): ClientRecord {
  return { ...sample("Board"), archive: [column] };
}

function record(value: unknown): ClientRecord {
  assert.ok(typeof value === "object" && value !== null && !Array.isArray(value), String(value));
  return value as ClientRecord;
}

function firstArchived(board: ClientRecord): ClientRecord {
  const archive = board["archive"];
  assert.ok(Array.isArray(archive), JSON.stringify(board));
  return record(archive[0]);
}

test("an optional member of a present group left out reads back absent", () => {
  const note = sample("Note");
  const meta = record(note["meta"]);
  delete meta["reviewer"];

  const read = roundTrip("Note", note);

  assert.ok(!("reviewer" in record(read["meta"])), JSON.stringify(read));
});

test("a member a present group requires reads back present when written empty", () => {
  const note = { ...sample("Note"), source: { url: "", excerpt: "Quoted." } };

  const read = roundTrip("Note", note);

  assert.deepEqual(read["source"], note.source);
});

test("an optional group left out reads back absent", () => {
  const note = sample("Note");
  delete note["source"];

  const read = roundTrip("Note", note);

  assert.ok(!("source" in read), JSON.stringify(read));
});

test("a member an item requires reads back present when written empty", () => {
  const board = archived({
    columnId: "archived-1",
    title: "",
    cards: [{ cardId: "card-1", text: "", done: false, note: { $mime: "text/plain", value: "Kept." } }]
  });

  const column = firstArchived(roundTrip("Board", board));

  assert.equal(column["title"], "");
  const cards = column["cards"];
  assert.ok(Array.isArray(cards));
  assert.equal(record(cards[0])["text"], "");
});

test("a sequence an item requires reads back empty when left out", () => {
  const column = firstArchived(roundTrip("Board", archived({ columnId: "archived-1", title: "Old" })));

  assert.deepEqual(column["cards"], []);
});

test("an optional member of an item left out reads back absent", () => {
  const board = archived({ columnId: "archived-1", title: "Old", cards: [] });

  const column = firstArchived(roundTrip("Board", board));

  assert.ok(!("wipLimit" in column), JSON.stringify(column));
});
