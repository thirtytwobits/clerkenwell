/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The React binding's entry point exports exactly the names its record
 * declares.
 */
import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  assertRecordedPublicApi,
  packagesNamedByExports
} from "../../client/tests/support/public-api";

test("the React binding exports exactly what public-api.json records", () => {
  assertRecordedPublicApi(path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."));
});

test("no type the React binding exports names a loro-crdt type", () => {
  const named = packagesNamedByExports(path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."));
  assert.equal(named.has("loro-crdt"), false, [...named].join(", "));
});
