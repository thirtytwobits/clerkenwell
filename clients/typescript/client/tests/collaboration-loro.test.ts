/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The plan-driven Loro replica: where each storage kind lives, what survives a
 * round trip, what a write refuses, and how replicas exchange updates.
 */
import assert from "node:assert/strict";
import test from "node:test";

import { resolveCollaborationContainer, type CollaborationFieldPlan } from "@clerkenwell/client";
import {
  collaborationDocumentToLoroDoc,
  collaborationReplicaSource,
  materializeCollaborationDocumentFromLoroDoc,
  requireCollaborationSchemaVersion
} from "@clerkenwell/client/loro";

import {
  BOARD_PLAN,
  boardDocument,
  clientSegment,
  clientValueAt,
  NOTE_PLAN,
  noteDocument,
  type BoardDocument,
  type NoteDocument
} from "./support/plans";
import { renameTask } from "./support/board-server";
import {
  boardReplica,
  boardReplicaFromUpdate,
  noteReplica,
  noteReplicaFromUpdate,
  roundTripBoard,
  roundTripNote
} from "./support/replicas";

const noteFields: readonly CollaborationFieldPlan[] = Object.values(NOTE_PLAN.fields);

function wireKeys(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(wireKeys);
  }
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, child]) => [
      key.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`),
      wireKeys(child)
    ]));
  }
  return value;
}

function withoutRevision<TDocument extends { etag?: string }>(document: TDocument): TDocument {
  const { etag: _etag, ...rest } = document;
  return rest as TDocument;
}

test("a document round-trips through an exported update unchanged", () => {
  assert.deepEqual(roundTripNote(noteDocument()), noteDocument());
  assert.deepEqual(roundTripBoard(boardDocument()), boardDocument());
});

test("every root field lives in the container and key its plan names", () => {
  const note = noteDocument();
  const doc = collaborationDocumentToLoroDoc(NOTE_PLAN, note);

  for (const field of noteFields) {
    const storage = field.storage;
    const value = clientValueAt(note, field.path);
    switch (storage.kind) {
      case "scalar":
        assert.deepEqual(doc.getMap(storage.container!).get(storage.key!), value, field.path);
        break;
      case "text":
        if (field.value.codec === "propertyText") {
          const property = value as NoteDocument["abstract"];
          assert.equal(doc.getText(storage.container!).toString(), property.value, field.path);
          assert.equal(
            doc.getMap(storage.metadataContainer!).get(storage.metadataKey!),
            property["$mime"],
            field.path
          );
        } else {
          assert.equal(doc.getText(storage.container!).toString(), value, field.path);
        }
        break;
      case "orderedList":
        assert.deepEqual(doc.getList(storage.container!).toArray(), value, field.path);
        break;
      case "structuredList":
        assert.deepEqual(doc.getList(storage.container!).toArray(), wireKeys(value), field.path);
        break;
      case "structuredMap":
        assert.deepEqual(doc.getMap(storage.container!).toJSON(), value, field.path);
        break;
      case "structuredDocument":
        assert.ok(doc.getMap(storage.container!).size > 0, field.path);
        break;
      case "derivedRevision":
        break;
      default:
        assert.fail(`The note plan has no ${storage.kind} field at the root.`);
    }
  }
});

test("a keyed sequence stores its identities in order and each item under its own containers", () => {
  const board = boardDocument();
  const doc = collaborationDocumentToLoroDoc(BOARD_PLAN, board);
  const columns = BOARD_PLAN.fields.columns.storage;
  const tasks = BOARD_PLAN.fields["columns.*.tasks"].storage;
  const taskTitle = BOARD_PLAN.fields["columns.*.tasks.*.title"].storage;
  const taskNotes = BOARD_PLAN.fields["columns.*.tasks.*.notes"].storage;

  assert.deepEqual(
    doc.getList(columns.orderContainer).toArray(),
    board.columns.map(({ columnId }) => columnId)
  );
  for (const column of board.columns) {
    const context = { column_id: column.columnId };
    assert.deepEqual(
      doc.getList(resolveCollaborationContainer(tasks.orderContainer, context)).toArray(),
      column.tasks.map(({ id }) => id)
    );
    for (const task of column.tasks) {
      const itemContext = { ...context, [tasks.identityVariable]: task.id };
      assert.equal(
        doc.getMap(resolveCollaborationContainer(taskTitle.containerTemplate, itemContext)).get(taskTitle.key),
        task.title
      );
      assert.equal(
        doc.getText(resolveCollaborationContainer(taskNotes.containerTemplate, itemContext)).toString(),
        task.notes
      );
    }
  }
});

test("a derived identity is read from the sequence, not stored as an item field", () => {
  const board = boardDocument();
  const doc = collaborationDocumentToLoroDoc(BOARD_PLAN, board);
  const column = board.columns[0]!;
  const item = doc.getMap(
    resolveCollaborationContainer(BOARD_PLAN.fields["columns.*.name"].storage.containerTemplate, {
      column_id: column.columnId
    })
  );

  assert.equal(item.get("column_id"), undefined);
  assert.equal(item.get(clientSegment("column_id")), undefined);
  assert.deepEqual(
    materializeCollaborationDocumentFromLoroDoc<BoardDocument>(doc, BOARD_PLAN, null).columns
      .map(({ columnId }) => columnId),
    board.columns.map(({ columnId }) => columnId)
  );
});

test("a derived revision is never stored and is supplied when materialising", () => {
  const revision = "loro:test-revision";
  const replica = noteReplica({ ...noteDocument(), etag: "written-by-a-client" });
  const reread = noteReplicaFromUpdate(replica.exportUpdateBase64());

  assert.equal(reread.currentDocument().etag, undefined);
  assert.equal(reread.materializedDocument(revision).etag, revision);
  assert.deepEqual(withoutRevision(reread.materializedDocument(revision)), noteDocument());
});

test("an absent optional field stays absent rather than acquiring content", () => {
  const note: NoteDocument = noteDocument();
  delete note.rating;
  delete note.subtitle;
  delete note.summary;
  delete note.attributes;
  delete note.outline;
  const board: BoardDocument = boardDocument();
  delete board.description;
  delete board.columns[0]!.tasks[0]!.estimateHours;

  const note2 = roundTripNote(note);
  const board2 = roundTripBoard(board);

  assert.deepEqual(note2, note);
  for (const field of ["rating", "subtitle", "summary", "attributes", "outline"]) {
    assert.ok(!(field in note2), field);
  }
  assert.deepEqual(board2, board);
  assert.ok(!("description" in board2));
  assert.ok(!("archive" in board2));
});

test("an optional string left empty comes back absent", () => {
  const note = { ...noteDocument(), subtitle: "", summary: "" };
  const board = { ...boardDocument(), description: "" };

  const note2 = roundTripNote(note);

  assert.ok(!("subtitle" in note2));
  assert.ok(!("summary" in note2));
  assert.ok(!("description" in roundTripBoard(board)));
});

test("an optional field in a present group comes back absent or empty, and keeps that shape", () => {
  const note = noteDocument();
  delete note.style.accentColour;

  const once = roundTripNote(note);
  const accent = once.style.accentColour;

  assert.ok(accent === undefined || accent === "", `accentColour came back as ${JSON.stringify(accent)}`);
  assert.equal(once.style.fontFamily, note.style.fontFamily);
  assert.deepEqual(roundTripNote(once), once);
});

test("an explicitly present empty map, document or optional sequence stays present", () => {
  const note = { ...noteDocument(), attributes: {}, outline: {} };
  const board = { ...boardDocument(), archive: [] };

  const note2 = roundTripNote(note);
  const board2 = roundTripBoard(board);

  assert.deepEqual(note2.attributes, {});
  assert.deepEqual(note2.outline, {});
  assert.deepEqual(board2.archive, []);
});

test("required empty lists and sequences round-trip empty", () => {
  const note = { ...noteDocument(), tags: [], links: [] };
  const board = {
    ...boardDocument(),
    columns: [{ columnId: "empty", name: "Empty", brief: "", tasks: [] }]
  };

  assert.deepEqual(roundTripNote(note), note);
  assert.deepEqual(roundTripBoard(board), board);
  assert.deepEqual(roundTripBoard({ ...board, columns: [] }).columns, []);
});

test("a structured list stores its entries by wire name and reads them back by client name", () => {
  const note = noteDocument();
  const doc = collaborationDocumentToLoroDoc(NOTE_PLAN, note);

  assert.deepEqual(doc.getList(NOTE_PLAN.fields.links.storage.container).toArray(), wireKeys(note.links));
  assert.deepEqual(roundTripNote(note).links, note.links);
});

test("a structured document keeps nested strings, identity-keyed lists and plain values", () => {
  const outline = {
    heading: "Agenda",
    depth: 2,
    flags: [true, false],
    items: [{ id: "a", text: "First" }, { id: "b", text: "Second", done: true }],
    tabs: [{ uid: "tab-1", label: "One" }],
    plain: [{ label: "no identity" }],
    nested: { caption: "Deep", tags: ["x", "y"] },
    reserved: { "$keyedBy": "id", note: "stored as a value" }
  };

  assert.deepEqual(roundTripNote({ ...noteDocument(), outline }).outline, outline);
});

test("each scalar codec refuses a value of the wrong type", () => {
  const invalid: readonly [string, Partial<Record<keyof NoteDocument, unknown>>][] = [
    ["integer", { priority: 1.5 }],
    ["number", { weight: Number.NaN }],
    ["number", { weight: Number.POSITIVE_INFINITY }],
    ["boolean", { pinned: "yes" }],
    ["string", { title: 7 }]
  ];
  for (const [codec, change] of invalid) {
    assert.throws(
      () => noteReplica({ ...noteDocument(), ...change } as NoteDocument),
      /Collaboration field/,
      codec
    );
  }
});

test("property text requires a text MIME type and a string value", () => {
  for (const abstract of [
    { "$mime": "application/json", value: "{}" },
    { "$mime": "text/plain", value: 3 },
    { value: "no MIME" },
    "bare string"
  ]) {
    assert.throws(
      () => noteReplica({ ...noteDocument(), abstract } as unknown as NoteDocument),
      /property value/,
      JSON.stringify(abstract)
    );
  }
  const plain = { "$mime": "text/plain", value: "Kept." };
  assert.deepEqual(roundTripNote({ ...noteDocument(), abstract: plain }).abstract, plain);
});

test("lists refuse entries of the wrong shape", () => {
  assert.throws(
    () => noteReplica({ ...noteDocument(), tags: ["ok", 3] } as unknown as NoteDocument),
    /string array/
  );
  assert.throws(
    () => noteReplica({ ...noteDocument(), links: ["not an object"] } as unknown as NoteDocument),
    /object array/
  );
});

test("every keyed item needs a unique, non-empty identity", () => {
  const board = boardDocument();
  const anonymous = structuredClone(board);
  anonymous.columns[0]!.columnId = "";
  const duplicated = structuredClone(board);
  duplicated.columns[1]!.columnId = duplicated.columns[0]!.columnId;
  const nestedDuplicate = structuredClone(board);
  nestedDuplicate.columns[0]!.tasks[1]!.id = nestedDuplicate.columns[0]!.tasks[0]!.id;

  assert.throws(() => boardReplica(anonymous), /identity/);
  assert.throws(() => boardReplica(duplicated), /duplicate identity/);
  assert.throws(() => boardReplica(nestedDuplicate), /duplicate identity/);
  assert.throws(
    () => boardReplica({ ...board, columns: "not a list" } as unknown as BoardDocument),
    /must be an array/
  );
});

test("a replica refuses an update from another schema version", () => {
  const replica = noteReplica(noteDocument());
  const update = replica.exportUpdateBase64();

  assert.doesNotThrow(() => requireCollaborationSchemaVersion("Note", NOTE_PLAN, NOTE_PLAN.schemaVersion));
  assert.throws(
    () => requireCollaborationSchemaVersion("Note", NOTE_PLAN, NOTE_PLAN.schemaVersion + 1),
    /schema version/
  );
  assert.throws(
    () => noteReplica(noteDocument()).importVersionedUpdateBase64(NOTE_PLAN.schemaVersion - 1, update),
    /schema version/
  );
  assert.doesNotThrow(
    () => noteReplica(noteDocument()).importVersionedUpdateBase64(NOTE_PLAN.schemaVersion, update)
  );
});

test("a replica starts from an accepted update when there is one, and needs content otherwise", () => {
  const update = noteReplica(noteDocument()).exportUpdateBase64();

  assert.deepEqual(collaborationReplicaSource("Note", noteDocument(), update), {
    kind: "update",
    updateBase64: update
  });
  assert.deepEqual(collaborationReplicaSource("Note", noteDocument(), undefined), {
    kind: "document",
    document: noteDocument()
  });
  assert.throws(() => collaborationReplicaSource("Note", undefined, undefined), /initial content/);
});

test("a fork edits under its own peer and its update applies to the parent", () => {
  const parent = boardReplica(boardDocument());
  const base = parent.acceptedFrontierBase64();
  const fork = parent.fork();
  const edited = structuredClone(fork.currentDocument());
  edited.columns[0]!.name = "Backlog";

  fork.replaceDocument(edited);

  assert.deepEqual(parent.currentDocument(), boardDocument());
  assert.equal(parent.coversFrontierBase64(fork.acceptedFrontierBase64()), false);
  assert.equal(parent.importUpdateBase64(fork.exportIncrementalUpdateBase64(base)), true);
  assert.equal(parent.coversFrontierBase64(fork.acceptedFrontierBase64()), true);
  assert.deepEqual(parent.currentDocument(), fork.currentDocument());
});

test("a replica covers its own frontier and importing ops it holds changes nothing", () => {
  const replica = boardReplica(boardDocument());

  assert.equal(replica.coversFrontierBase64(replica.acceptedFrontierBase64()), true);
  assert.equal(replica.importUpdateBase64(replica.exportUpdateBase64()), false);
});

test("rewriting an unchanged document emits no ops", () => {
  const replica = boardReplica(boardDocument());
  const base = replica.acceptedFrontierBase64();

  replica.replaceDocument(replica.currentDocument());

  const peer = boardReplicaFromUpdate(replica.exportUpdateBase64());
  assert.equal(peer.importUpdateBase64(replica.exportIncrementalUpdateBase64(base)), false);
});

test("an incremental update carries the edit, not the document", () => {
  const board = boardDocument();
  for (let index = 0; index < 20; index += 1) {
    board.columns[0]!.tasks.push({
      id: `filler-${index}`,
      title: `Filler ${index}`,
      notes: "Padding the board so one edit is small beside it.",
      labels: ["filler"]
    });
  }
  const replica = boardReplica(board);
  const base = replica.acceptedFrontierBase64();
  const accepted = boardReplicaFromUpdate(replica.exportUpdateBase64());
  const edited = structuredClone(replica.currentDocument());
  edited.columns[1]!.tasks[0]!.title = "Pick a later date";

  replica.replaceDocument(edited);

  const update = replica.exportIncrementalUpdateBase64(base);
  assert.equal(accepted.importUpdateBase64(update), true);
  assert.deepEqual(accepted.currentDocument(), edited);
  assert.ok(update.length < replica.exportUpdateBase64().length / 4);
});

test("adopting a document changes what the replica reports, not what it holds", () => {
  const replica = noteReplica(noteDocument());
  const update = replica.exportUpdateBase64();
  const adopted = { ...noteDocument(), title: "Presented differently" };

  replica.adoptDocument(adopted);

  assert.deepEqual(replica.currentDocument(), adopted);
  assert.equal(replica.exportUpdateBase64(), update);
  assert.deepEqual(noteReplicaFromUpdate(replica.exportUpdateBase64()).currentDocument(), noteDocument());
});

test("exporting past an update's frontier carries later edits to a replica built from that update", () => {
  const accepted = boardReplica(boardDocument());
  const update = accepted.exportUpdateBase64();
  const local = boardReplicaFromUpdate(update);
  const edited = renameTask(local.currentDocument(), "task-2", "Book the hall");
  local.replaceDocument(edited);

  const pending = local.exportIncrementalUpdateBase64(local.updateFrontierBase64(update));
  const elsewhere = boardReplicaFromUpdate(update);
  elsewhere.importUpdateBase64(pending);

  assert.equal(local.coversFrontierBase64(local.updateFrontierBase64(update)), true);
  assert.deepEqual(elsewhere.currentDocument(), edited);
});
