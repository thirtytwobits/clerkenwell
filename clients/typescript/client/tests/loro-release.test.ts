/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Properties of the pinned `loro-crdt` release that the replica depends on.
 */
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

import { LoroDoc } from "loro-crdt";

test("replaying accepted text history keeps WASM memory bounded", () => {
  const require = createRequire(import.meta.url);
  const { __wasm } = require("loro-crdt/nodejs/loro_wasm.js") as {
    __wasm: { memory: WebAssembly.Memory };
  };
  const source = new LoroDoc();
  const text = "A long thought about London. 🦊\n".repeat(4000);
  source.getText("text").insert(0, text);
  source.commit();
  const update = source.export({ mode: "update" });
  const receiver = new LoroDoc();
  for (let index = 0; index < 32; index += 1) receiver.import(update);
  const warmedBytes = __wasm.memory.buffer.byteLength;

  for (let index = 0; index < 512; index += 1) receiver.import(update);

  const growth = __wasm.memory.buffer.byteLength - warmedBytes;
  assert.ok(growth <= 4 * 1024 * 1024, `Replaying unchanged history retained ${growth} additional WASM bytes`);
  assert.equal(receiver.getText("text").toString(), text);
  receiver.free();
  source.free();
});

test("a caret before a surviving emoji keeps a valid UTF-16 anchor after a deletion before it", () => {
  const doc = new LoroDoc();
  const text = doc.getText("text");
  const prefix = "fore";
  text.insert(0, `${prefix} 🦊 after`);
  doc.commit();
  text.delete(prefix.length, 1);
  doc.commit();

  const cursor = text.getCursor(prefix.length, 1);

  assert.ok(cursor, "A caret before a surviving emoji must have a stable anchor.");
  assert.equal(doc.getCursorPos(cursor)?.offset, prefix.length);
});
