/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * One session survives an edit, a dismissal, a restart and a reconnect, and
 * its operation is accepted exactly once.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  AuthoringRuntime,
  authoringLeaveDecision
} from "@clerkenwell/client";

test("edit, dismiss, restart, reconnect, and exactly-once replay retain one session", () => {
  const resource = { entity: "Note", resourceKey: "restartable" } as const;
  const beforeRestart = new AuthoringRuntime();
  beforeRestart.open({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "base",
    supportedExchangeModes: ["incremental", "bootstrap"],
    baseline: { text: "accepted" },
    draft: { text: "accepted" }
  });
  beforeRestart.modify(resource, { text: "offline edit" });
  beforeRestart.queue(resource, {
    operationId: "stable-operation",
    schemaVersion: 1,
    exchangeMode: "incremental",
    baseRevision: "base",
    updateBase64: "incremental-update"
  });
  beforeRestart.disconnect(resource);

  const durable = beforeRestart.session<{ text: string }>(resource);
  assert.ok(durable);
  assert.equal(authoringLeaveDecision(durable, true).kind, "allow");

  const afterRestart = new AuthoringRuntime();
  afterRestart.restore(beforeRestart.getSnapshot());
  afterRestart.reconnect(resource);
  afterRestart.beginReplay(resource, "stable-operation");
  afterRestart.acknowledge({
    resource,
    operationId: "stable-operation",
    document: { text: "offline edit" },
    acceptedRevision: "accepted"
  });
  afterRestart.acknowledge({
    resource,
    operationId: "stable-operation",
    document: { text: "offline edit" },
    acceptedRevision: "accepted"
  });

  const converged = afterRestart.session<{ text: string }>(resource);
  assert.ok(converged);
  assert.equal(converged.status, "clean");
  assert.deepEqual(converged.baseline, converged.draft);
  assert.equal(converged.queuedOperations.length, 0);
});
