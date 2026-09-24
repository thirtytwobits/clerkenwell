/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The WASM side of the Loro release compatibility test in
 * `crates/clerkenwell-conformance`. It loads the `loro-crdt` that
 * `@clerkenwell/client` resolves, imports a snapshot and any updates from a
 * JSON request on standard input, optionally inserts a prefix into the `text`
 * container, checks every UTF-16 cursor boundary and a snapshot reopen, and
 * writes the value, a snapshot, an update and the loaded package version.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";

interface Request {
  readonly snapshot: string;
  readonly updates?: readonly string[];
  readonly prefix?: string;
}

const client = createRequire(new URL("../clients/typescript/client/package.json", import.meta.url));
const entry = client.resolve("loro-crdt");
const { LoroDoc } = await import(pathToFileURL(entry).href) as typeof import("loro-crdt");

function installedVersion(from: string): string {
  for (let directory = path.dirname(from); directory !== path.dirname(directory); directory = path.dirname(directory)) {
    try {
      const manifest = JSON.parse(readFileSync(path.join(directory, "package.json"), "utf8")) as {
        readonly name?: string;
        readonly version?: string;
      };
      if (manifest.name === "loro-crdt" && manifest.version !== undefined) return manifest.version;
    } catch {
      continue;
    }
  }
  throw new Error(`No loro-crdt package.json encloses ${from}.`);
}

const request = JSON.parse(readFileSync(0, "utf8")) as Request;
const doc = new LoroDoc();
doc.import(Buffer.from(request.snapshot, "base64"));
doc.setPeerId("303");
for (const update of request.updates ?? []) doc.import(Buffer.from(update, "base64"));
const text = doc.getText("text");
if (request.prefix) {
  text.insert(0, request.prefix);
  doc.commit();
}
const value = text.toString();
const boundaries = new Set<number>([0]);
let offset = 0;
for (const scalar of value) {
  offset += scalar.length;
  boundaries.add(offset);
}
for (let position = 0; position <= value.length; position++) {
  for (const side of [-1, 0, 1] as const) {
    const cursor = text.getCursor(position, side);
    if (boundaries.has(position)) {
      assert.ok(cursor, `Missing UTF-16 cursor at ${position}`);
      assert.equal(doc.getCursorPos(cursor)?.offset, position);
    } else {
      assert.equal(cursor, undefined, "A cursor cannot anchor inside a surrogate pair");
    }
  }
}
const snapshot = doc.export({ mode: "snapshot" });
const reopened = new LoroDoc();
reopened.import(snapshot);
assert.deepEqual(reopened.toJSON(), doc.toJSON());
assert.deepEqual(reopened.version().toJSON(), doc.version().toJSON());
process.stdout.write(JSON.stringify({
  version: installedVersion(entry),
  value: doc.toJSON(),
  snapshot: Buffer.from(snapshot).toString("base64"),
  update: Buffer.from(doc.export({ mode: "update" })).toString("base64")
}));
