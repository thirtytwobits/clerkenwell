/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Subscriptions over a transport: one-shot subscribe, and a watch that keeps a
 * materialised projection through reconnects, stale events and resync.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  PROJECTION_UPDATE_NOTIFICATION,
  subscribeProjection,
  watchProjection,
  type ProjectionConnectionState,
  type ProjectionNotification,
  type ProjectionTransport,
  type ProjectionTransportEvent
} from "@clerkenwell/client";

import {
  TEST_COMPOSITION_PLANS,
  type NoteSummary,
  type TestProjectionModel
} from "./support/projections";

type Model = TestProjectionModel;

class RefusedRequest extends Error {}

const tick = () => new Promise<void>((resolve) => setImmediate(resolve));

function transportHarness() {
  const listeners = new Set<(notification: ProjectionNotification) => void>();
  const connections = new Set<(state: ProjectionConnectionState) => void>();
  const pending: Array<{
    resolve: (value: { subscription_id: number; revision: number }) => void;
    reject: (error: unknown) => void;
  }> = [];
  const released: number[] = [];
  const resynced: number[] = [];
  const client: ProjectionTransport<Model> = {
    addNotificationListener: (listener) => {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    addConnectionStateListener: (listener) => {
      connections.add(listener);
      return () => { connections.delete(listener); };
    },
    projectionSubscribe: async () => new Promise((resolve, reject) => pending.push({ resolve, reject })),
    projectionUnsubscribe: async (id) => { released.push(id); return { removed: true }; },
    projectionResync: async (id) => {
      resynced.push(id);
      return { subscription_id: id, from_revision: 1, revision: 2 };
    },
    isRefusal: (error) => error instanceof RefusedRequest
  };
  const emit = (params: ProjectionTransportEvent<Model>) => listeners.forEach((listener) =>
    listener({ method: PROJECTION_UPDATE_NOTIFICATION, params }));
  const snapshot = (id: number, revision: number, notes: NoteSummary[]) => emit({
    kind: "snapshot",
    subscription_id: id,
    revision,
    snapshot: { projection: "notes.list", value: { notes } }
  });
  const connection = (status: string) => connections.forEach((listener) => listener({ status }));
  return { client, pending, released, resynced, listeners, connections, emit, snapshot, connection };
}

const accepted = [{ note_id: "accepted", title: "Accepted" }];

test("a watch consumes an early snapshot and ignores stale, duplicate and foreign revisions", async () => {
  const h = transportHarness();
  const values: NoteSummary[][] = [];
  const errors: unknown[] = [];
  const ready = watchProjection({
    client: h.client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.list",
    params: {},
    onValue: (value) => values.push(value.notes),
    onError: (error) => errors.push(error)
  });
  await tick();
  h.snapshot(1, 2, accepted);
  h.pending[0]!.resolve({ subscription_id: 1, revision: 2 });
  const watch = await ready;
  h.snapshot(1, 1, [{ note_id: "stale", title: "Stale" }]);
  h.snapshot(1, 2, [{ note_id: "duplicate", title: "Duplicate" }]);
  h.snapshot(2, 3, [{ note_id: "foreign", title: "Another subscription" }]);

  assert.deepEqual(values, [accepted]);
  assert.deepEqual(errors, []);
  await watch.unsubscribe();
  assert.deepEqual(h.released, [1]);
  assert.equal(h.listeners.size + h.connections.size, 0);
});

test("a connection opening during subscribe creates only one subscription", async () => {
  const h = transportHarness();
  const subscribe = h.client.projectionSubscribe;
  h.client.projectionSubscribe = (...args) => {
    h.connection("connecting");
    h.connection("connected");
    return subscribe(...args);
  };
  const values: NoteSummary[][] = [];
  const ready = watchProjection({
    client: h.client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.list",
    params: {},
    onValue: (value) => values.push(value.notes),
    onError: (error) => assert.fail(String(error))
  });
  await tick();
  assert.equal(h.pending.length, 1);
  h.snapshot(1, 1, accepted);
  h.pending[0]!.resolve({ subscription_id: 1, revision: 1 });
  const watch = await ready;
  assert.deepEqual(values, [accepted]);
  await watch.unsubscribe();
});

test("a reconnect while subscribe is in flight keeps the new connection's subscription", async () => {
  const h = transportHarness();
  const values: NoteSummary[][] = [];
  const ready = watchProjection({
    client: h.client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.list",
    params: {},
    onValue: (value) => values.push(value.notes),
    onError: (error) => assert.fail(String(error))
  });
  await tick();
  h.connection("disconnected");
  h.connection("connected");
  await tick();
  assert.equal(h.pending.length, 2);
  h.pending[0]!.resolve({ subscription_id: 1, revision: 1 });
  await tick();
  h.snapshot(1, 1, accepted);
  h.pending[1]!.resolve({ subscription_id: 1, revision: 1 });
  const watch = await ready;
  assert.deepEqual(values, [accepted]);
  assert.deepEqual(h.released, []);
  await watch.unsubscribe();
});

test("a missing revision requests resync and keeps the last accepted value", async () => {
  const h = transportHarness();
  const values: NoteSummary[][] = [];
  const errors: unknown[] = [];
  const ready = watchProjection({
    client: h.client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.list",
    params: {},
    onValue: (value) => values.push(value.notes),
    onError: (error) => errors.push(error)
  });
  await tick();
  h.snapshot(1, 1, accepted);
  h.pending[0]!.resolve({ subscription_id: 1, revision: 1 });
  const watch = await ready;
  h.emit({
    kind: "patch",
    subscription_id: 1,
    from_revision: 2,
    to_revision: 3,
    patch: { projection: "notes.list", value: { kind: "remove", note_id: "accepted" } }
  });
  assert.deepEqual(values, [accepted]);
  assert.equal(errors.length, 1);
  assert.deepEqual(h.resynced, [1]);

  const recovered = [{ note_id: "recovered", title: "Recovered" }];
  h.snapshot(1, 4, recovered);
  assert.deepEqual(values, [accepted, recovered]);
  await watch.unsubscribe();
});

test("a watch folds patches through the projection's composition plan", async () => {
  const h = transportHarness();
  const values: NoteSummary[][] = [];
  const patches: unknown[] = [];
  const ready = watchProjection({
    client: h.client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.list",
    params: {},
    onValue: (value, patch) => { values.push(value.notes); patches.push(patch); },
    onError: (error) => assert.fail(String(error))
  });
  await tick();
  h.pending[0]!.resolve({ subscription_id: 1, revision: 1 });
  await tick();
  h.snapshot(1, 1, accepted);
  await ready;
  const added = { note_id: "added", title: "Added" };
  const upsert = { kind: "upsert", summary: added } as const;
  h.emit({
    kind: "patch",
    subscription_id: 1,
    from_revision: 1,
    to_revision: 2,
    patch: { projection: "notes.list", value: upsert }
  });

  assert.deepEqual(values, [accepted, [...accepted, added]]);
  assert.deepEqual(patches, [undefined, upsert]);
});

test("a refused subscribe fails the watch; a lost one waits for the next connection", async () => {
  const refused = transportHarness();
  const refusedWatch = watchProjection({
    client: refused.client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.list",
    params: {},
    onValue: () => assert.fail("A refused subscription published a value."),
    onError: () => undefined
  });
  await tick();
  const refusal = new RefusedRequest("Unknown projection.");
  refused.pending[0]!.reject(refusal);
  await assert.rejects(refusedWatch, (error) => error === refusal);

  const lost = transportHarness();
  const values: NoteSummary[][] = [];
  const lostWatch = watchProjection({
    client: lost.client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.list",
    params: {},
    onValue: (value) => values.push(value.notes),
    onError: () => undefined
  });
  await tick();
  lost.pending[0]!.reject(new Error("The connection closed."));
  await tick();
  lost.connection("connected");
  await tick();
  assert.equal(lost.pending.length, 2);
  lost.pending[1]!.resolve({ subscription_id: 7, revision: 1 });
  await tick();
  lost.snapshot(7, 1, accepted);
  const watch = await lostWatch;
  assert.deepEqual(values, [accepted]);
  await watch.unsubscribe();
});

test("an aborted watch releases its subscription and rejects", async () => {
  const h = transportHarness();
  const controller = new AbortController();
  const ready = watchProjection({
    client: h.client,
    plans: TEST_COMPOSITION_PLANS,
    signal: controller.signal,
    projection: "notes.list",
    params: {},
    onValue: () => undefined,
    onError: () => undefined
  });
  await tick();
  h.pending[0]!.resolve({ subscription_id: 3, revision: 1 });
  await tick();
  controller.abort();

  await assert.rejects(ready, /cancelled/);
  assert.deepEqual(h.released, [3]);
  assert.equal(h.listeners.size + h.connections.size, 0);
});

test("a watch honours a caller-owned initial snapshot deadline", async () => {
  const h = transportHarness();
  const ready = watchProjection({
    client: h.client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.list",
    params: {},
    initialSnapshotTimeoutMs: 1,
    onValue: () => assert.fail("A snapshot was not published."),
    onError: () => undefined
  });
  await assert.rejects(ready, /Timed out waiting for projection snapshot/);
});

test("a one-shot subscription resolves its first snapshot, reports patches, and releases on unsubscribe", async () => {
  const h = transportHarness();
  const patches: unknown[] = [];
  const snapshots: NoteSummary[][] = [];
  const subscribing = subscribeProjection<Model, "notes.list">({
    client: h.client,
    projection: "notes.list",
    params: {},
    onSnapshot: (value) => snapshots.push(value.notes),
    onPatch: (patch) => patches.push(patch)
  });
  await tick();
  h.snapshot(4, 0, accepted);
  h.pending[0]!.resolve({ subscription_id: 4, revision: 0 });
  const subscription = await subscribing;

  assert.equal(subscription.subscriptionId, 4);
  assert.deepEqual(subscription.snapshot.notes, accepted);
  assert.deepEqual(snapshots, [accepted]);

  const remove = { kind: "remove", note_id: "accepted" } as const;
  h.emit({
    kind: "patch",
    subscription_id: 4,
    from_revision: 0,
    to_revision: 1,
    patch: { projection: "notes.list", value: remove }
  });
  h.emit({
    kind: "patch",
    subscription_id: 5,
    from_revision: 0,
    to_revision: 1,
    patch: { projection: "notes.list", value: { kind: "reset", notes: [] } }
  });
  assert.deepEqual(patches, [remove]);

  await subscription.unsubscribe();
  assert.deepEqual(h.released, [4]);
  assert.equal(h.listeners.size, 0);
});

for (const arrival of ["before", "after"] as const) {
  test(`a one-shot subscription whose snapshot handler throws on a snapshot arriving ${arrival} the accept fails with that error`, async () => {
    const h = transportHarness();
    const failure = new Error("The handler refused the snapshot.");
    const subscribing = subscribeProjection<Model, "notes.list">({
      client: h.client,
      projection: "notes.list",
      params: {},
      onSnapshot: () => { throw failure; }
    });
    const failed = assert.rejects(subscribing, (error) => error === failure);
    await tick();
    if (arrival === "before") h.snapshot(6, 0, accepted);
    h.pending[0]!.resolve({ subscription_id: 6, revision: 0 });
    await tick();
    if (arrival === "after") h.snapshot(6, 0, accepted);

    await failed;
    assert.equal(h.listeners.size, 0);
    assert.deepEqual(h.released, [6]);
  });
}

test("a one-shot subscription that fails releases its listener", async () => {
  const h = transportHarness();
  const subscribing = subscribeProjection({
    client: h.client,
    projection: "notes.list",
    params: {}
  });
  await tick();
  const failure = new RefusedRequest("Refused.");
  h.pending[0]!.reject(failure);

  await assert.rejects(subscribing, (error) => error === failure);
  assert.equal(h.listeners.size, 0);
  assert.deepEqual(h.released, []);
});
