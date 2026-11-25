/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The generated TypeScript types of the schema subset's nullable values and
 * tagged unions admit what their schemas admit, and the contract fixtures
 * are values of them.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import type * as Notebook from "../../../../crates/clerkenwell-notebook/generated/typescript/index.js";

/** `true` exactly when every `A` is a `B`. */
type Assignable<A, B> = [A] extends [B] ? true : false;

const fixtures = JSON.parse(
  readFileSync(
    new URL("../../../../crates/clerkenwell-notebook/generated/notebook.projections.fixtures.json", import.meta.url),
    "utf8"
  )
) as { projections: { projection: string; snapshot: { value: unknown } }[] };

test("a nullable member admits null beside its values, and an optional one may be absent", () => {
  const admitted: readonly true[] = [
    true satisfies Assignable<null, Notebook.SyncHealthRecord["last_attempt_ms"]>,
    true satisfies Assignable<number, Notebook.SyncHealthRecord["last_attempt_ms"]>,
    true satisfies Assignable<undefined, Notebook.SyncHealthRecord["retry_after_ms"]>,
    true satisfies Assignable<null, Notebook.SyncHealthRecord["resume_cursor"]>,
    true satisfies Assignable<null, Notebook.SyncHealthRecord["failure"]>,
    true satisfies Assignable<null, Notebook.SyncNote>
  ];
  const refused: readonly false[] = [
    false satisfies Assignable<undefined, Notebook.SyncHealthRecord["last_attempt_ms"]>,
    false satisfies Assignable<string, Notebook.SyncHealthRecord["last_attempt_ms"]>
  ];
  assert.ok(admitted.every(Boolean));
  assert.ok(refused.every((holds) => !holds));
});

test("an externally tagged union is its one-member objects and its unit tags", () => {
  const failures: Notebook.SyncFailure[] = [
    { transport: { attempts: 1, status: null } },
    { refused: { reason: "busy" } },
    { conflict: { peer: "p2", since: 4 } },
    { silent: null },
    { retried: "cancelled" },
    "expired"
  ];
  const refused: readonly false[] = [
    false satisfies Assignable<"unknown", Notebook.SyncFailure>,
    false satisfies Assignable<{ refused: { reason: number } }, Notebook.SyncFailure>
  ];
  assert.equal(failures.length, 6);
  assert.ok(refused.every((holds) => !holds));
});

test("an internally tagged union's open variants admit members beside the tag", () => {
  const cursors: Notebook.SyncCursor[] = [
    { position: "start", hint: [1] },
    { position: "at", offset: 3, hint: "x" },
    { position: "end" }
  ];
  const refused: readonly false[] = [
    false satisfies Assignable<{ position: "at" }, Notebook.SyncCursor>,
    false satisfies Assignable<{ position: "elsewhere" }, Notebook.SyncCursor>
  ];
  assert.equal(cursors.length, 3);
  assert.ok(refused.every((holds) => !holds));
});

test("the sync health fixture is a value of its generated type", () => {
  const fixture = fixtures.projections.find((projection) => projection.projection === "sync.health");
  assert.ok(fixture);
  const record = fixture.snapshot.value as Notebook.SyncHealthRecord;
  assert.ok(record.phase === "idle" || record.phase === "syncing");
  assert.ok("failure" in record);
  assert.ok(typeof record.cursor.position === "string");
});
