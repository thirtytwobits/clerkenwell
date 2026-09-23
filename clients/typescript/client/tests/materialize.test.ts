/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The materialiser folds each subscription's patches into its last snapshot
 * by the strategy its composition plan names, and refuses anything that does
 * not continue the revision sequence it holds.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  applyProjectionEvent,
  createProjectionMaterializerState,
  ProjectionMaterializerError,
  type ProjectionMaterializerState,
  type ProjectionName,
  type ProjectionPatch,
  type ProjectionSnapshot
} from "@clerkenwell/client";

import {
  TEST_COMPOSITION_PLANS,
  type NoteRecord,
  type NoteSummary,
  type TestProjectionModel
} from "./support/projections";

type Model = TestProjectionModel;
type State = ProjectionMaterializerState<Model>;

function createState(): State {
  return createProjectionMaterializerState<Model>(TEST_COMPOSITION_PLANS);
}

function open<K extends ProjectionName<Model>>(
  state: State,
  subscriptionId: number,
  projection: K,
  snapshot: ProjectionSnapshot<Model, K>,
  revision = 0
) {
  return applyProjectionEvent(state, {
    kind: "snapshot",
    subscription_id: subscriptionId,
    revision,
    snapshot: { projection, value: snapshot }
  } as Parameters<typeof applyProjectionEvent<Model>>[1]);
}

function patch<K extends ProjectionName<Model>>(
  state: State,
  subscriptionId: number,
  projection: K,
  value: ProjectionPatch<Model, K>,
  fromRevision: number,
  toRevision = fromRevision + 1
) {
  return applyProjectionEvent(state, {
    kind: "patch",
    subscription_id: subscriptionId,
    from_revision: fromRevision,
    to_revision: toRevision,
    patch: { projection, value }
  } as Parameters<typeof applyProjectionEvent<Model>>[1]);
}

function snapshotOf<K extends ProjectionName<Model>>(
  state: State,
  subscriptionId: number,
  projection: K
): ProjectionSnapshot<Model, K> | null {
  const materialized = state.subscriptions.get(subscriptionId);
  assert.ok(materialized);
  assert.equal(materialized.projection, projection);
  return materialized.value.snapshot as ProjectionSnapshot<Model, K> | null;
}

const ada: NoteSummary = { note_id: "ada", title: "Ada" };
const bert: NoteSummary = { note_id: "bert", title: "Bert" };

test("a snapshot opens a subscription at its revision and is returned as materialised", () => {
  const state = createState();
  const snapshot = { notes: [ada] };
  const revision = 4;

  const update = open(state, 7, "notes.list", snapshot, revision);

  assert.equal(update.projection, "notes.list");
  assert.equal(update.value.subscription_id, 7);
  assert.equal(update.value.revision, revision);
  assert.deepEqual(update.value.snapshot, snapshot);
  assert.equal(state.subscriptions.get(7), update);
});

test("a keyed collection upserts by identity, removes by identity and resets whole", () => {
  const state = createState();
  open(state, 1, "notes.list", { notes: [ada] });

  patch(state, 1, "notes.list", { kind: "upsert", summary: bert }, 0);
  assert.deepEqual(snapshotOf(state, 1, "notes.list")?.notes, [ada, bert]);

  const renamedAda = { ...ada, title: "Ada Lovelace" };
  patch(state, 1, "notes.list", { kind: "upsert", summary: renamedAda }, 1);
  const afterRename = snapshotOf(state, 1, "notes.list")?.notes ?? [];
  assert.deepEqual(afterRename.filter(({ note_id }) => note_id === ada.note_id), [renamedAda]);
  assert.equal(afterRename.length, 2);

  patch(state, 1, "notes.list", { kind: "remove", note_id: bert.note_id }, 2);
  assert.deepEqual(snapshotOf(state, 1, "notes.list")?.notes, [renamedAda]);

  const reset = [bert];
  patch(state, 1, "notes.list", { kind: "reset", notes: reset }, 3);
  assert.deepEqual(snapshotOf(state, 1, "notes.list")?.notes, reset);
  assert.equal(state.subscriptions.get(1)?.value.revision, 4);
});

test("a keyed collection refuses an item without its identity field", () => {
  const state = createState();
  open(state, 1, "notes.list", { notes: [] });

  assert.throws(
    () => patch(state, 1, "notes.list", { kind: "upsert", summary: { title: "anonymous" } } as never, 0),
    ProjectionMaterializerError
  );
});

test("replace-or-remove replaces from the patch, merges updates and nulls the removed field", () => {
  const state = createState();
  const note: NoteRecord = { title: "Minutes", body: "Short." };
  open(state, 2, "notes.byId", { note: null });

  patch(state, 2, "notes.byId", { kind: "replace", note_id: "ada", note }, 0);
  assert.deepEqual(snapshotOf(state, 2, "notes.byId"), { note });

  const changes = { title: "Revised minutes" };
  patch(state, 2, "notes.byId", { kind: "update", changes }, 1);
  assert.deepEqual(snapshotOf(state, 2, "notes.byId")?.note, { ...note, ...changes });

  patch(state, 2, "notes.byId", { kind: "remove", note_id: "ada" }, 2);
  assert.equal(snapshotOf(state, 2, "notes.byId")?.note, null);
});

test("replace-or-remove in field mode takes the named field and removes to a null snapshot", () => {
  const state = createState();
  const accepted = { update_base64: "AAEC" };
  open(state, 3, "notes.authoringState", null);

  patch(state, 3, "notes.authoringState", { kind: "replace", state: accepted }, 0);
  assert.deepEqual(snapshotOf(state, 3, "notes.authoringState"), accepted);

  patch(state, 3, "notes.authoringState", { kind: "remove" }, 1);
  assert.equal(snapshotOf(state, 3, "notes.authoringState"), null);
});

test("replace takes the patch or its named field and refuses any other patch kind", () => {
  const state = createState();
  open(state, 4, "boards.summary", { count: 1 });
  open(state, 5, "boards.detail", { title: "Before" });

  patch(state, 4, "boards.summary", { kind: "replace", count: 2 }, 0);
  assert.deepEqual(snapshotOf(state, 4, "boards.summary"), { count: 2 });

  const board = { title: "After" };
  patch(state, 5, "boards.detail", { kind: "replace", board }, 0);
  assert.deepEqual(snapshotOf(state, 5, "boards.detail"), board);

  assert.throws(
    () => patch(state, 4, "boards.summary", { kind: "remove" } as never, 1),
    ProjectionMaterializerError
  );
});

test("reset replaces the snapshot with the patch and refuses any other patch kind", () => {
  const state = createState();
  open(state, 6, "activity.log", { entries: ["opened"] });
  const entries = ["opened", "edited"];

  patch(state, 6, "activity.log", { kind: "reset", entries }, 0);
  assert.deepEqual(snapshotOf(state, 6, "activity.log"), { entries });

  assert.throws(
    () => patch(state, 6, "activity.log", { kind: "append", entries } as never, 1),
    ProjectionMaterializerError
  );
});

test("sequenced text appends each output delta in sequence to its item", () => {
  const state = createState();
  open(state, 8, "runs.transcript", { runs: [{ run_id: "run-1" }, { run_id: "run-2" }] });
  const deltas = ["Once ", "upon ", "a time."];

  deltas.forEach((text, index) => {
    patch(state, 8, "runs.transcript", {
      kind: "output",
      run_id: "run-1",
      delta: { sequence: index + 1, text }
    }, index);
  });

  const runs = snapshotOf(state, 8, "runs.transcript")?.runs ?? [];
  assert.deepEqual(runs.find(({ run_id }) => run_id === "run-1")?.output, {
    sequence: deltas.length,
    text: deltas.join("")
  });
  assert.equal(runs.find(({ run_id }) => run_id === "run-2")?.output, undefined);
});

test("sequenced text refuses a skipped sequence and an unknown item", () => {
  const state = createState();
  open(state, 9, "runs.transcript", { runs: [{ run_id: "run-1" }] });

  assert.throws(
    () => patch(state, 9, "runs.transcript", {
      kind: "output",
      run_id: "run-1",
      delta: { sequence: 2, text: "skipped" }
    }, 0),
    ProjectionMaterializerError
  );
  assert.throws(
    () => patch(state, 9, "runs.transcript", {
      kind: "output",
      run_id: "missing",
      delta: { sequence: 1, text: "orphan" }
    }, 0),
    ProjectionMaterializerError
  );
});

test("sequenced text resets to the patch", () => {
  const state = createState();
  open(state, 10, "runs.transcript", { runs: [{ run_id: "run-1" }] });
  const runs = [{ run_id: "run-2", output: { sequence: 1, text: "Hello" } }];

  patch(state, 10, "runs.transcript", { kind: "reset", runs }, 0);

  assert.deepEqual(snapshotOf(state, 10, "runs.transcript"), { runs });
});

test("a patch must continue its subscription's revision sequence", () => {
  const state = createState();
  const snapshot = { notes: [ada] };
  open(state, 11, "notes.list", snapshot, 5);
  const upsert = { kind: "upsert", summary: bert } as const;

  assert.throws(() => patch(state, 12, "notes.list", upsert, 5), ProjectionMaterializerError);
  assert.throws(() => patch(state, 11, "notes.list", upsert, 4), ProjectionMaterializerError);
  assert.throws(() => patch(state, 11, "notes.list", upsert, 5, 5), ProjectionMaterializerError);
  assert.throws(
    () => patch(state, 11, "activity.log", { kind: "reset", entries: [] }, 5),
    ProjectionMaterializerError
  );

  assert.deepEqual(snapshotOf(state, 11, "notes.list"), snapshot);
  assert.equal(state.subscriptions.get(11)?.value.revision, 5);
});

test("a refused patch leaves the materialised value where it was", () => {
  const state = createState();
  const snapshot = { runs: [{ run_id: "run-1", output: { sequence: 1, text: "kept" } }] };
  open(state, 13, "runs.transcript", structuredClone(snapshot), 2);

  assert.throws(() => patch(state, 13, "runs.transcript", {
    kind: "output",
    run_id: "run-1",
    delta: { sequence: 3, text: "out of order" }
  }, 2));

  assert.deepEqual(snapshotOf(state, 13, "runs.transcript"), snapshot);
  assert.equal(state.subscriptions.get(13)?.value.revision, 2);
});

test("a patch for a projection without a composition plan is refused", () => {
  const state = createProjectionMaterializerState({});
  applyProjectionEvent(state, {
    kind: "snapshot",
    subscription_id: 1,
    revision: 0,
    snapshot: { projection: "unplanned", value: {} }
  });

  assert.throws(
    () => applyProjectionEvent(state, {
      kind: "patch",
      subscription_id: 1,
      from_revision: 0,
      to_revision: 1,
      patch: { projection: "unplanned", value: { kind: "replace" } }
    }),
    ProjectionMaterializerError
  );
});
