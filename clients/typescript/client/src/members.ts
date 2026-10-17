/**
 * A document's members as its own properties. A member name is data, so
 * `__proto__`, `constructor` and the like name members like any other, never
 * what an object inherits.
 */

/** `record`'s own member `name`, or `undefined` when it has none. */
export function ownMember(record: object, name: string): unknown {
  return Object.prototype.hasOwnProperty.call(record, name)
    ? (record as Record<string, unknown>)[name]
    : undefined;
}

/** Sets `record`'s own member `name` to `value`, whatever `name` is. */
export function defineMember(record: object, name: string, value: unknown): void {
  Object.defineProperty(record, name, {
    value,
    writable: true,
    enumerable: true,
    configurable: true
  });
}
