/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Structural equality for JSON-shaped authoring documents.
 */

/**
 * Compares arrays in order and objects by property name, independent of the
 * order in which projection materialisers inserted those properties.
 * Undefined object properties are equivalent to absent properties, matching
 * JSON serialisation.
 */
export function areJsonValuesEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) {
    return true;
  }
  if (left === null || right === null) {
    return false;
  }
  if (Array.isArray(left)) {
    if (!Array.isArray(right) || left.length !== right.length) {
      return false;
    }
    return left.every((value, index) => areJsonValuesEqual(value, right[index]));
  }
  if (Array.isArray(right)) {
    return false;
  }
  if (typeof left !== "object" || typeof right !== "object") {
    return false;
  }

  const leftRecord = left as Record<string, unknown>;
  const rightRecord = right as Record<string, unknown>;
  const leftKeys = Object.keys(leftRecord).filter((key) => leftRecord[key] !== undefined);
  const rightKeys = Object.keys(rightRecord).filter((key) => rightRecord[key] !== undefined);
  if (leftKeys.length !== rightKeys.length) {
    return false;
  }
  return leftKeys.every(
    (key) => Object.hasOwn(rightRecord, key)
      && rightRecord[key] !== undefined
      && areJsonValuesEqual(leftRecord[key], rightRecord[key])
  );
}
