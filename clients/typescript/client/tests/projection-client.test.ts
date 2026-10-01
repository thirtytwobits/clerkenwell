/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The projection protocol over a JSON-RPC socket.
 */
import assert from "node:assert/strict";
import test from "node:test";

import { ProjectionClient, RpcError, watchProjection } from "@clerkenwell/client";

import { fakeSocket, settle } from "./support/fake-socket.js";
import { TEST_COMPOSITION_PLANS, type TestProjectionModel } from "./support/projections.js";

function projectionClient() {
  const harness = fakeSocket();
  return { ...harness, client: new ProjectionClient<TestProjectionModel>(harness.socket) };
}

test("a subscription names its projection, its params and what the client holds", async () => {
  const { client, latest } = projectionClient();
  const held = { frontier_base64: "AAE=", etag: "loro:4" };

  const subscribed = client.projectionSubscribe("notes.byId", { note_id: "n1" }, { held });
  latest().accept();
  await settle();
  latest().answer(0, { subscription_id: 7, revision: 5, up_to_date: true });

  assert.deepEqual(await subscribed, { subscription_id: 7, revision: 5, up_to_date: true });
  assert.deepEqual(latest().request(0), {
    jsonrpc: "2.0",
    id: latest().request(0).id,
    method: "projection.subscribe",
    params: { projection: "notes.byId", params: { note_id: "n1" }, held }
  });
});

test("a mutation resolves with the result it produced", async () => {
  const { client, latest } = projectionClient();
  const renamed = { note_id: "n1", title: "Errands" };

  const mutated = client.projectionMutate("note.rename", renamed, { operationId: "rename-1" });
  latest().accept();
  await settle();
  latest().answer(0, { operation_id: "rename-1", revision: 2, result: { mutation: "note.rename", value: renamed } });

  assert.deepEqual(await mutated, renamed);
  assert.deepEqual(latest().request(0).params, { mutation: "note.rename", params: renamed, operation_id: "rename-1" });
});

test("a mutation answered with another mutation's result is rejected", async () => {
  const { client, latest } = projectionClient();

  const mutated = client.projectionMutate("note.rename", { note_id: "n1", title: "Errands" });
  latest().accept();
  await settle();
  latest().answer(0, { revision: 2, result: { mutation: "note.delete", value: {} } });

  await assert.rejects(mutated, /note\.rename/);
});

test("a refusal is told apart from a request lost with its connection", async () => {
  const { client, latest } = projectionClient();

  const refused = client.projectionResync(3);
  const lost = client.projectionUnsubscribe(3);
  latest().accept();
  await settle();
  latest().refuseRequest(0, { code: -32004, message: "No subscription." });
  latest().drop();

  const refusal = await refused.catch((error: unknown) => error);
  const loss = await lost.catch((error: unknown) => error);
  assert.ok(refusal instanceof RpcError);
  assert.equal(client.isRefusal(refusal), true);
  assert.equal(client.isRefusal(loss), false);
  await client.socket.disconnect();
});

test("a watch keeps a projection current through its snapshot and patches", async () => {
  const { client, latest } = projectionClient();
  const values: unknown[] = [];
  const connected = client.socket.connect();
  latest().accept();
  await connected;

  const watching = watchProjection<TestProjectionModel, "notes.byId">({
    client,
    plans: TEST_COMPOSITION_PLANS,
    projection: "notes.byId",
    params: { note_id: "n1" },
    onValue: (value) => values.push(value),
    onError: (error) => assert.fail(String(error))
  });
  await settle();
  latest().answer(0, { subscription_id: 1, revision: 1 });
  const note = { title: "Shopping", body: "Bread." };
  latest().push({
    jsonrpc: "2.0",
    method: "projection.update",
    params: { kind: "snapshot", subscription_id: 1, revision: 1, snapshot: { projection: "notes.byId", value: { note_id: "n1", note } } }
  });
  const watch = await watching;
  latest().push({
    jsonrpc: "2.0",
    method: "projection.update",
    params: {
      kind: "patch", subscription_id: 1, from_revision: 1, to_revision: 2,
      patch: { projection: "notes.byId", value: { kind: "update", changes: { title: "Errands" } } }
    }
  });
  await settle();

  assert.equal(values.length, 2);
  assert.deepEqual(values.at(-1), { note_id: "n1", note: { ...note, title: "Errands" } });
  const unsubscribed = watch.unsubscribe();
  await settle();
  latest().answer(1, { removed: true });
  await unsubscribed;
  await client.socket.disconnect();
});
