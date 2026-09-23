/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Two documents are equal when they serialise to the same JSON, whatever order
 * their properties were built in.
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { areJsonValuesEqual } from "@clerkenwell/client";

const json = fc.jsonValue();

function reordered(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(reordered);
  }
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value).reverse().map(([key, child]) => [key, reordered(child)])
    );
  }
  return value;
}

test("equality agrees with JSON serialisation of key-sorted values", () => {
  const canonical = (value: unknown): string => JSON.stringify(value, (_key, child: unknown) =>
    child !== null && typeof child === "object" && !Array.isArray(child)
      ? Object.fromEntries(Object.entries(child).sort(([left], [right]) => left.localeCompare(right)))
      : child);
  fc.assert(
    fc.property(json, json, (left, right) => {
      assert.equal(areJsonValuesEqual(left, right), canonical(left) === canonical(right));
    }),
    { numRuns: 300 }
  );
});

test("property order never affects equality", () => {
  fc.assert(
    fc.property(json, (value) => {
      assert.equal(areJsonValuesEqual(value, reordered(value)), true);
      assert.equal(areJsonValuesEqual(reordered(value), value), true);
    }),
    { numRuns: 300 }
  );
});

test("an undefined property is the same as an absent one", () => {
  fc.assert(
    fc.property(fc.dictionary(fc.string(), json), fc.string(), (record, key) => {
      const withoutKey = Object.fromEntries(Object.entries(record).filter(([name]) => name !== key));
      assert.equal(areJsonValuesEqual({ ...withoutKey, [key]: undefined }, withoutKey), true);
    }),
    { numRuns: 200 }
  );
});
