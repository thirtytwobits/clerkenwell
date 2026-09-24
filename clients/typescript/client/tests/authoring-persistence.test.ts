/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The persisted authoring snapshot carries pending work, not the whole runtime.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  AuthoringRuntime,
  authoringSessionRequiresDurableRestoration,
  durableSessions,
  fromPersistedRuntime,
  sameSessions,
  toPersistedRuntime
} from "@clerkenwell/client";

const resource = { entity: "Note", resourceKey: "persisted" } as const;
const other = { entity: "Note", resourceKey: "untouched" } as const;

function openedRuntime(): AuthoringRuntime {
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental", "bootstrap"],
    baseline: { prose: "server copy" },
    draft: { prose: "server copy" }
  });
  return runtime;
}

function persistedKeys(runtime: AuthoringRuntime): string[] {
  return Object.keys(toPersistedRuntime(runtime.getSnapshot()).sessions);
}

test("a session the server can supply again is left out of the snapshot", () => {
  const runtime = openedRuntime();
  const opened = runtime.session(resource);

  assert.ok(opened);
  assert.equal(authoringSessionRequiresDurableRestoration(opened), false);
  assert.deepEqual(persistedKeys(runtime), []);
});

test("a diverged draft is persisted, and stops being persisted once discarded", () => {
  const runtime = openedRuntime();
  runtime.modify(resource, { prose: "local edit" });

  const dirty = runtime.session(resource);
  assert.ok(dirty);
  assert.equal(authoringSessionRequiresDurableRestoration(dirty), true);
  assert.deepEqual(persistedKeys(runtime), [`${resource.entity}:${resource.resourceKey}`]);

  runtime.discard(resource);

  assert.deepEqual(persistedKeys(runtime), []);
});

test("a queued operation is persisted even once its draft matches the baseline", () => {
  const runtime = openedRuntime();
  runtime.modify(resource, { prose: "local edit" });
  runtime.queue(resource, {
    operationId: "operation",
    schemaVersion: 1,
    exchangeMode: "incremental",
    baseRevision: "accepted",
    updateBase64: "update"
  });
  runtime.modify(resource, { prose: "server copy" });

  const queued = runtime.session(resource);
  assert.ok(queued);
  assert.deepEqual(queued.baseline, queued.draft);
  assert.equal(queued.queuedOperations.length, 1);
  assert.equal(authoringSessionRequiresDurableRestoration(queued), true);
  assert.deepEqual(persistedKeys(runtime), [`${resource.entity}:${resource.resourceKey}`]);
});

test("a restart restores pending work and nothing else", () => {
  const beforeRestart = openedRuntime();
  beforeRestart.modify(resource, { prose: "local edit" });
  beforeRestart.open({
    resource: other,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental", "bootstrap"],
    baseline: { prose: "server copy" },
    draft: { prose: "server copy" }
  });

  const afterRestart = new AuthoringRuntime(
    fromPersistedRuntime(toPersistedRuntime(beforeRestart.getSnapshot()))
  );

  assert.deepEqual(
    afterRestart.session<{ prose: string }>(resource)?.draft,
    beforeRestart.session<{ prose: string }>(resource)?.draft
  );
  assert.equal(afterRestart.session(other), undefined);
});

test("restoring a snapshot does not drop a live session the snapshot omits", () => {
  const runtime = openedRuntime();
  const live = runtime.session(resource);

  // What the provider echoes back after any publish: the runtime's own state,
  // narrowed to pending work. A clean session must survive its own round trip.
  runtime.restore(fromPersistedRuntime(toPersistedRuntime(runtime.getSnapshot())));

  assert.deepEqual(runtime.session(resource), live);
});

function optimisticRuntime(): AuthoringRuntime {
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource,
    policy: "optimisticDocument",
    schemaVersion: 1,
    acceptedRevision: "etag",
    supportedExchangeModes: ["optimisticDocument"],
    baseline: { prose: "server copy" },
    draft: { prose: "server copy" }
  });
  return runtime;
}

test("restoring adopts another runtime's optimistic draft over the live session", () => {
  const runtime = optimisticRuntime();
  const elsewhere = optimisticRuntime();
  elsewhere.modify(resource, { prose: "edited in another window" });

  runtime.restore(fromPersistedRuntime(toPersistedRuntime(elsewhere.getSnapshot())));

  assert.deepEqual(
    runtime.session<{ prose: string }>(resource)?.draft,
    elsewhere.session<{ prose: string }>(resource)?.draft
  );
});

test("restoring carries another runtime's collaborative session without adopting it", () => {
  const runtime = openedRuntime();
  const live = runtime.session(resource);
  const elsewhere = openedRuntime();
  elsewhere.modify(resource, { prose: "edited in another window" });
  const persisted = toPersistedRuntime(elsewhere.getSnapshot());

  runtime.restore(fromPersistedRuntime(persisted));

  assert.equal(runtime.session(resource), live);
  assert.deepEqual(toPersistedRuntime(runtime.persistedState()), persisted);
});

test("this runtime's own pending work is persisted over a carried session", () => {
  const runtime = openedRuntime();
  const elsewhere = openedRuntime();
  elsewhere.modify(resource, { prose: "edited in another window" });
  runtime.restore(fromPersistedRuntime(toPersistedRuntime(elsewhere.getSnapshot())));

  runtime.modify(resource, { prose: "edited here" });

  assert.deepEqual(
    toPersistedRuntime(runtime.persistedState()),
    toPersistedRuntime(runtime.getSnapshot())
  );
});

test("a carried session is dropped once a later snapshot omits it", () => {
  const runtime = openedRuntime();
  const elsewhere = openedRuntime();
  elsewhere.modify(resource, { prose: "edited in another window" });
  runtime.restore(fromPersistedRuntime(toPersistedRuntime(elsewhere.getSnapshot())));

  elsewhere.discard(resource);
  runtime.restore(fromPersistedRuntime(toPersistedRuntime(elsewhere.getSnapshot())));

  assert.deepEqual(persistedKeys(runtime), []);
  assert.deepEqual(Object.keys(toPersistedRuntime(runtime.persistedState()).sessions), []);
});

test("a publish that leaves the durable sessions alone produces the same persisted set", () => {
  const runtime = openedRuntime();
  runtime.modify(resource, { prose: "local edit" });
  const before = durableSessions(runtime.getSnapshot());
  assert.equal(before.size, 1);

  runtime.open({
    resource: other,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental", "bootstrap"],
    baseline: { prose: "server copy" },
    draft: { prose: "server copy" }
  });

  assert.equal(sameSessions(durableSessions(runtime.getSnapshot()), before), true);

  runtime.modify(resource, { prose: "another edit" });
  assert.equal(sameSessions(durableSessions(runtime.getSnapshot()), before), false);

  runtime.discard(resource);
  assert.equal(sameSessions(durableSessions(runtime.getSnapshot()), before), false);
  assert.equal(durableSessions(runtime.getSnapshot()).size, 0);
});
