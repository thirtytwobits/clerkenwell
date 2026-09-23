/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Update frames travel as base64 JSON fields and must survive the trip exactly.
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { base64ToBytes, bytesToBase64 } from "@clerkenwell/client";

test("any byte sequence round-trips through base64", () => {
  fc.assert(
    fc.property(fc.uint8Array({ maxLength: 512 }), (bytes) => {
      const encoded = bytesToBase64(bytes);
      assert.match(encoded, /^[A-Za-z0-9+/]*={0,2}$/);
      assert.deepEqual(base64ToBytes(encoded), bytes);
    }),
    { numRuns: 200 }
  );
});
