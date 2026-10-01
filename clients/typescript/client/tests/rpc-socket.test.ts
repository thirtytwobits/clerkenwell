/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * JSON-RPC over one WebSocket: responses settle their requests, notifications
 * reach listeners, and the socket reopens with backoff after it closes.
 */
import assert from "node:assert/strict";
import test from "node:test";

import { RpcError, type RpcSocketState } from "@clerkenwell/client";

import { fakeSocket, pause, settle } from "./support/fake-socket.js";

test("a request resolves with the result its response carries", async () => {
  const { socket, latest } = fakeSocket();

  const called = socket.call("notes.count", { folder: "inbox" });
  latest().accept();
  await settle();
  const sent = latest().request(0);
  latest().answer(0, { count: 3 });

  assert.deepEqual(await called, { count: 3 });
  assert.equal(sent.method, "notes.count");
  assert.deepEqual(sent.params, { folder: "inbox" });
});

test("a refused request rejects with the error and envelope its response carries", async () => {
  const { socket, latest } = fakeSocket();
  const envelope = { code: "unknown_mutation", message: "No.", operation: "mutate", retryable: false };

  const called = socket.call("projection.mutate", { mutation: "note.explode", params: {} });
  latest().accept();
  await settle();
  latest().refuseRequest(0, { code: -32602, message: "No.", data: { projection_error: envelope } });

  const error = await called.then(() => assert.fail("the request resolved"), (error: unknown) => error);
  assert.ok(error instanceof RpcError);
  assert.equal(error.code, -32602);
  assert.deepEqual(error.projectionError, envelope);
});

test("a refused request rejects with the error the application builds for it", async () => {
  class ServerRefusal extends RpcError {}
  const { socket, latest } = fakeSocket({ refusal: (error) => new ServerRefusal(error) });

  const called = socket.call("notes.delete", { note_id: "n1" });
  latest().accept();
  await settle();
  latest().refuseRequest(0, { code: -32004, message: "No such note." });

  const error = await called.then(() => assert.fail("the request resolved"), (error: unknown) => error);
  assert.ok(error instanceof ServerRefusal);
  assert.equal(error.message, "No such note.");
});

test("each response settles its own request, whatever order they arrive in", async () => {
  const { socket, latest } = fakeSocket();

  const first = socket.call("first");
  const second = socket.call("second");
  latest().accept();
  await settle();
  latest().answer(1, "second's");
  latest().answer(0, "first's");

  assert.deepEqual(await Promise.all([first, second]), ["first's", "second's"]);
});

test("a notification reaches every listener and settles no request", async () => {
  const { socket, latest } = fakeSocket();
  const heard: Array<[string, unknown]> = [];
  socket.addNotificationListener(({ method, params }) => heard.push([method, params]));
  socket.addNotificationListener(({ method }) => heard.push([method, "again"]));
  let settled = false;
  const called = socket.call("slow").finally(() => { settled = true; });
  latest().accept();
  await settle();

  latest().push({ jsonrpc: "2.0", method: "projection.update", params: { kind: "patch" } });
  await settle();

  assert.deepEqual(heard, [["projection.update", { kind: "patch" }], ["projection.update", "again"]]);
  assert.equal(settled, false);
  latest().answer(0, null);
  await called;
});

test("a request pending when the socket closes is rejected, and the socket reopens", async () => {
  let reopened = 0;
  const { socket, sockets, latest } = fakeSocket({
    closedMessage: "Closed.",
    onReopen: () => { reopened += 1; }
  });
  const states: RpcSocketState[] = [];
  socket.addConnectionStateListener((state) => states.push(state));
  const called = socket.call("slow");
  latest().accept();
  await settle();

  latest().drop();

  await assert.rejects(called, { message: "Closed." });
  assert.equal(socket.getConnectionState().status, "retry_wait");
  assert.ok((socket.getConnectionState().retryAt ?? 0) >= Date.now());
  while (sockets.length < 2) await pause(1);
  latest().accept();
  assert.equal(socket.getConnectionState().status, "connected");
  assert.equal(reopened, 1);
  assert.deepEqual(
    states.map(({ status }) => status),
    ["disconnected", "connecting", "connected", "retry_wait", "connecting", "connected"]
  );
});

test("each failed attempt to reopen waits longer than the last", async () => {
  const { socket, sockets, latest } = fakeSocket();
  const waits: number[] = [];
  socket.addConnectionStateListener(({ status, retryAt }) => {
    if (status === "retry_wait" && retryAt !== null) waits.push(retryAt - Date.now());
  });
  const connected = socket.connect();
  latest().accept();
  await connected;

  latest().drop();
  for (const attempt of [2, 3]) {
    while (sockets.length < attempt) await pause(1);
    latest().refuse();
    await settle();
    await settle();
  }

  assert.ok(waits.length >= 3, `${waits}`);
  assert.ok(waits[1]! > waits[0]!, `${waits}`);
  assert.ok(waits[2]! > waits[1]!, `${waits}`);
  await socket.disconnect();
});

test("a socket that cannot be reached rejects the attempt to open it", async () => {
  const { socket, latest } = fakeSocket({ reconnectOnClose: false, unreachableMessage: "Unreachable." });

  const connected = socket.connect();
  latest().refuse();

  await assert.rejects(connected, { message: "Unreachable." });
  await settle();
  assert.equal(socket.getConnectionState().status, "disconnected");
});

test("a socket disconnected on purpose stays closed", async () => {
  const { socket, sockets, latest } = fakeSocket();
  const connected = socket.connect();
  latest().accept();
  await connected;

  await socket.disconnect();
  await pause(30);

  assert.equal(sockets.length, 1);
  assert.equal(socket.getConnectionState().status, "disconnected");
});
