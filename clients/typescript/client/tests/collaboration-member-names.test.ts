/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A member name is data. A replica holds `__proto__` as a document member like
 * any other name, at any depth and in either direction, and never reaches what
 * an object inherits.
 */
import assert from "node:assert/strict";
import test from "node:test";
import { peerBlock } from "./support/peers.js";

import type { CollaborationEntityPlan } from "@clerkenwell/client";
import { CollaborationReplica } from "@clerkenwell/client/replica";

const PLAN = {
  schemaVersion: 1,
  authoringState: { projection: "probes.authoringState", importMutation: "probe.importUpdate" },
  rootContainer: "probe",
  fields: {
    "__proto__.label": {
      path: "__proto__.label",
      storage: { kind: "scalar", container: "probe", key: "__proto__.label" },
      value: { codec: "string" },
      required: true,
      requiredInParent: true,
      conflict: "explicit"
    },
    "meta.__proto__": {
      path: "meta.__proto__",
      storage: { kind: "scalar", container: "probe", key: "meta.__proto__" },
      value: { codec: "string" },
      required: true,
      requiredInParent: true,
      conflict: "explicit"
    },
    outline: {
      path: "outline",
      storage: { kind: "structuredDocument", container: "outline" },
      value: { codec: "structuredJson" },
      required: true,
      requiredInParent: true,
      conflict: "merge"
    }
  }
} satisfies CollaborationEntityPlan;

/** A document whose members, at every level, include `__proto__` as data. */
function probe(): Record<string, unknown> {
  return JSON.parse(`{
    "__proto__": { "label": "intermediate" },
    "meta": { "__proto__": "final" },
    "outline": {
      "__proto__": { "polluted": "nested" },
      "items": [{ "id": "__proto__", "text": "keyed" }, { "id": "plain", "text": "other" }]
    }
  }`) as Record<string, unknown>;
}

function assertPrototypeUntouched(): void {
  const fresh: Record<string, unknown> = {};
  for (const name of ["label", "polluted", "text", "id"]) {
    assert.equal(fresh[name], undefined, `Object.prototype gained ${name}`);
  }
}

test("a replica holds __proto__ as a member at every depth, written and read", () => {
  const document = probe();
  const written = CollaborationReplica.from("Probe", PLAN, { kind: "document", document }, peerBlock());
  const read = CollaborationReplica.from<Record<string, unknown>>("Probe", PLAN, {
    kind: "update",
    updateBase64: written.exportUpdateBase64()
  }, peerBlock());

  const held = read.currentDocument();

  assertPrototypeUntouched();
  assert.deepEqual(held, document);
  assert.equal(Object.getPrototypeOf(held), Object.prototype);
  assert.ok(Object.prototype.hasOwnProperty.call(held, "__proto__"), "an own __proto__ member");
});

test("an edit beneath __proto__ is written as a member, not onto a prototype", () => {
  const replica = CollaborationReplica.from<Record<string, unknown>>(
    "Probe",
    PLAN,
    { kind: "document", document: probe() },
    peerBlock()
  );
  const next = probe();
  (next["__proto__"] as Record<string, unknown>).label = "changed";
  (next["meta"] as Record<string, unknown>)["__proto__"] = "changed";
  replica.replaceDocument(next);

  assertPrototypeUntouched();
  assert.deepEqual(replica.currentDocument(), next);
});
