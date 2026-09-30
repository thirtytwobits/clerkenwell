/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The client's entry points export exactly the names its record declares,
 * name nothing after Loro, and export no Loro type.
 */
import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { assertRecordedPublicApi, namesAfterLoro, packagesNamedByExports } from "./support/public-api";

test("the client's entry points export exactly what public-api.json records", () => {
  assertRecordedPublicApi(path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."));
});

test("no type the client's entry points export names a loro-crdt type", () => {
  const named = packagesNamedByExports(path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."));
  assert.equal(named.has("loro-crdt"), false, [...named].join(", "));
});

test("nothing the client's entry points export is named after Loro", () => {
  assert.deepEqual(namesAfterLoro(path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..")), []);
});
