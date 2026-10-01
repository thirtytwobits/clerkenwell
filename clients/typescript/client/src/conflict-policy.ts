/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Field conflict policy evaluation over client documents, judging as the
 * Rust evaluator in `clerkenwell-doc` does: `immutable` fields may not change
 * at all, `explicit` fields refuse two different changes to one value, and
 * `merge` and `lastWriterWins` fields always accept. Fields inside a keyed
 * sequence are judged per item, addressed by the item's identity.
 */
import { areJsonValuesEqual } from "./json-value-equality.js";
import {
  clientSegment,
  type CollaborationEntityPlan,
  type CollaborationFieldPlan
} from "./plans.js";

/**
 * The declared field paths an edit from `base` to `client` changes against
 * their conflict policy, given that the accepted document has meanwhile
 * become `current`. Documents are in client naming; paths are reported in
 * wire naming, a keyed item's field as `sequence[identity].field`.
 */
export function conflictingFieldPaths(
  plan: CollaborationEntityPlan,
  base: unknown,
  client: unknown,
  current: unknown
): string[] {
  const conflicts = new Set<string>();
  for (const field of Object.values(plan.fields)) {
    if (
      (field.conflict !== "explicit" && field.conflict !== "immutable")
      || field.storage.kind === "derivedIdentity"
      || field.storage.kind === "derivedRevision"
    ) {
      continue;
    }
    const baseValues = fieldValues(plan, field, base);
    const clientValues = fieldValues(plan, field, client);
    const currentValues = fieldValues(plan, field, current);
    const keys = new Set([...baseValues.keys(), ...clientValues.keys(), ...currentValues.keys()]);
    for (const key of keys) {
      const baseValue = baseValues.get(key) ?? null;
      const clientValue = clientValues.get(key) ?? null;
      const currentValue = currentValues.get(key) ?? null;
      const changed = !areJsonValuesEqual(clientValue, baseValue);
      const conflict = field.conflict === "immutable"
        ? changed
        : changed
          && !areJsonValuesEqual(currentValue, baseValue)
          && !areJsonValuesEqual(clientValue, currentValue);
      if (conflict) {
        conflicts.add(key);
      }
    }
  }
  return [...conflicts].sort();
}

function fieldValues(
  plan: CollaborationEntityPlan,
  field: CollaborationFieldPlan,
  document: unknown
): Map<string, unknown> {
  const values = new Map<string, unknown>();
  collectFieldValues(plan, "", field.path, document, "", values);
  return values;
}

/**
 * Collects the values `relative` addresses in `scope`, whose declared path
 * prefix is `declaredPrefix`, descending through every keyed sequence on the
 * way. Each value is keyed by its field path with every enclosing item named
 * by its identity: `sequence[identity].nested[identity].field`.
 */
function collectFieldValues(
  plan: CollaborationEntityPlan,
  declaredPrefix: string,
  relative: string,
  scope: unknown,
  keyPrefix: string,
  values: Map<string, unknown>
): void {
  const split = relative.indexOf(".*.");
  if (split < 0) {
    values.set(`${keyPrefix}${relative}`, valueAtPath(scope, relative) ?? null);
    return;
  }
  const sequenceRelative = relative.slice(0, split);
  const rest = relative.slice(split + 3);
  const sequencePath = `${declaredPrefix}${sequenceRelative}`;
  const sequence = plan.fields[sequencePath];
  const identityPath = sequence?.storage.kind === "keyedSequence"
    ? sequence.storage.identityPath
    : undefined;
  const items = valueAtPath(scope, sequenceRelative);
  if (identityPath === undefined || !Array.isArray(items)) {
    return;
  }
  for (const item of items) {
    const identity = valueAtPath(item, identityPath);
    if (typeof identity !== "string") {
      continue;
    }
    collectFieldValues(
      plan,
      `${sequencePath}.*.`,
      rest,
      item,
      `${keyPrefix}${sequenceRelative}[${identity}].`,
      values
    );
  }
}

/** The value at a wire path in a client document. */
function valueAtPath(value: unknown, path: string): unknown {
  let current = value;
  for (const segment of path.split(".").filter((part) => part.length > 0)) {
    if (current === null || typeof current !== "object" || Array.isArray(current)) {
      return undefined;
    }
    current = (current as Record<string, unknown>)[clientSegment(segment)];
  }
  return current;
}
