/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Diff-based writes hold exactly what was written, and materialisation settles
 * on one shape.
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { noteDocument, type NoteDocument } from "./support/plans";
import { noteReplica, noteReplicaFromUpdate, roundTripNote } from "./support/replicas";

const entry = fc.constantFrom("a", "b", "c", "d", "e", "f");

test("a list rewritten any number of times holds exactly the entries last written", () => {
  fc.assert(
    fc.property(fc.array(fc.array(entry, { maxLength: 8 }), { minLength: 1, maxLength: 12 }), (rewrites) => {
      const replica = noteReplica(noteDocument());
      for (const tags of rewrites) {
        replica.replaceDocument({ ...replica.currentDocument(), tags });
        const reread = noteReplicaFromUpdate(replica.exportUpdateBase64());
        assert.deepEqual(reread.currentDocument().tags, tags);
      }
    }),
    { numRuns: 60 }
  );
});

test("text rewritten any number of times holds exactly the text last written", () => {
  fc.assert(
    fc.property(fc.array(fc.string({ maxLength: 16 }), { minLength: 1, maxLength: 10 }), (bodies) => {
      const replica = noteReplica(noteDocument());
      for (const body of bodies) {
        replica.replaceDocument({ ...replica.currentDocument(), body });
      }
      assert.equal(noteReplicaFromUpdate(replica.exportUpdateBase64()).currentDocument().body, bodies.at(-1));
    }),
    { numRuns: 60 }
  );
});

const OPTIONAL_FIELDS = ["rating", "subtitle", "summary", "attributes", "outline"] as const;

test("materialising a document with any optional fields left out settles on one shape", () => {
  fc.assert(
    fc.property(fc.subarray([...OPTIONAL_FIELDS]), fc.boolean(), (omitted, omitAccent) => {
      const note: NoteDocument = noteDocument();
      for (const field of omitted) {
        delete note[field];
      }
      if (omitAccent) {
        delete note.style.accentColour;
      }
      const once = roundTripNote(note);
      for (const field of omitted) {
        assert.ok(!(field in once), `${field} acquired content`);
      }
      assert.deepEqual(roundTripNote(once), once);
    }),
    { numRuns: 40 }
  );
});
