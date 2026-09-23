/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A queued operation is replayed only when the ground it was built on still
 * holds; every other case resolves to a block an operator can act on.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  authoringReplayDecision,
  type QueuedAuthoringOperation
} from "@clerkenwell/client";

const SCHEMA_VERSION = 1;

function queuedOperation(
  overrides: Partial<QueuedAuthoringOperation<{ key: string }>> = {}
): QueuedAuthoringOperation<{ key: string }> {
  return {
    operationId: "operation-1",
    resource: { entity: "Board", resourceKey: "shared-board" },
    schemaVersion: SCHEMA_VERSION,
    exchangeMode: "incremental",
    baseRevision: "frontier-base64",
    document: { key: "shared-board" },
    retryCount: 0,
    state: "queued",
    ...overrides
  };
}

test("a causally valid incremental operation replays in its own exchange mode", () => {
  assert.deepEqual(
    authoringReplayDecision(queuedOperation(), SCHEMA_VERSION),
    { kind: "replay", exchangeMode: "incremental" }
  );
});

test("a bootstrap operation keeps its exchange mode rather than becoming incremental", () => {
  assert.deepEqual(
    authoringReplayDecision(
      queuedOperation({ exchangeMode: "bootstrap", baseRevision: "" }),
      SCHEMA_VERSION
    ),
    { kind: "replay", exchangeMode: "bootstrap" }
  );
});

test("operations that are not queued are skipped rather than replayed twice", () => {
  for (const state of ["sending", "blocked"] as const) {
    assert.deepEqual(
      authoringReplayDecision(queuedOperation({ state }), SCHEMA_VERSION),
      { kind: "skip" },
      `state ${state}`
    );
  }
});

test("an operation from another schema version blocks on migration", () => {
  assert.deepEqual(
    authoringReplayDecision(
      queuedOperation({ schemaVersion: SCHEMA_VERSION + 1 }),
      SCHEMA_VERSION
    ),
    { kind: "blocked", reason: "migrationBlocked" }
  );
});

test("an incremental operation with no base frontier blocks on its dependency", () => {
  assert.deepEqual(
    authoringReplayDecision(queuedOperation({ baseRevision: "" }), SCHEMA_VERSION),
    { kind: "blocked", reason: "dependencyBlocked" }
  );
});

test("an operation whose document did not survive requires recovery", () => {
  assert.deepEqual(
    authoringReplayDecision(queuedOperation({ document: undefined }), SCHEMA_VERSION),
    { kind: "blocked", reason: "recoveryRequired" }
  );
});
