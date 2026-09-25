/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The authoring state machine: every transition keeps local intent until the
 * server has accepted it.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  AuthoringRuntime,
  acknowledgeAuthoringOperation,
  adoptAuthoringBaseline,
  authoringLeaveDecision,
  authoringSessionAcceptsDraft,
  beginAuthoringReplay,
  blockAuthoringSession,
  disconnectAuthoringSession,
  modifyAuthoringSession,
  queueAuthoringOperation,
  redactedAuthoringRuntimeDiagnostic,
  reconnectAuthoringSession,
  rejectAuthoringOperation,
  resolveAuthoringDraftForBaselineAdoption,
  startAuthoringSession,
  supersedeBlockedAuthoringOperations,
  type AuthoringSessionStatus
} from "@clerkenwell/client";


const resource = { entity: "Note", resourceKey: "note" } as const;

const EVERY_STATUS = Object.keys({
  bootstrapping: true,
  live: true,
  clean: true,
  modified: true,
  commitPending: true,
  commitRejected: true,
  disconnectedReadable: true,
  offlineModified: true,
  replaying: true,
  migrationBlocked: true,
  dependencyBlocked: true,
  policyConflict: true,
  resyncRequired: true,
  recoveryRequired: true
} satisfies Record<AuthoringSessionStatus, true>) as AuthoringSessionStatus[];

function modifiedSession() {
  return modifyAuthoringSession(
    startAuthoringSession({
      resource,
      policy: "collaborative",
      schemaVersion: 1,
      acceptedRevision: "accepted",
      supportedExchangeModes: ["incremental", "bootstrap"],
      baseline: { prose: "baseline" },
      draft: { prose: "baseline" }
    }),
    { prose: "first edit" }
  );
}

function queuedSession() {
  return queueAuthoringOperation(modifiedSession(), {
    operationId: "operation",
    schemaVersion: 1,
    exchangeMode: "incremental",
    baseRevision: "accepted",
    updateBase64: "update"
  });
}

test("an acknowledgement cannot erase an edit made while the commit is in flight", () => {
  const sending = beginAuthoringReplay(queuedSession(), "operation");
  const editedAgain = modifyAuthoringSession(sending, { prose: "later edit" });
  const accepted = acknowledgeAuthoringOperation({
    session: editedAgain,
    operationId: "operation",
    document: { prose: "first edit" },
    acceptedRevision: "next"
  });

  assert.deepEqual(accepted.baseline, { prose: "first edit" });
  assert.deepEqual(accepted.draft, { prose: "later edit" });
  assert.equal(accepted.status, "modified");
  assert.equal(accepted.queuedOperations.length, 0);
});

test("an acknowledgement clears dirty state when a projection reorders object properties", () => {
  const localDocument = {
    section: {
      ownerId: "pip",
      goal: "Make room for the retro."
    }
  };
  const acceptedDocument = {
    section: {
      goal: "Make room for the retro.",
      ownerId: "pip"
    }
  };
  const queued = queueAuthoringOperation(
    modifyAuthoringSession(
      startAuthoringSession({
        resource,
        policy: "collaborative",
        schemaVersion: 1,
        acceptedRevision: "accepted",
        supportedExchangeModes: ["incremental"],
        baseline: { section: { ownerId: "pip", goal: "Wait." } },
        draft: localDocument
      }),
      localDocument
    ),
    {
      operationId: "reordered-projection",
      schemaVersion: 1,
      exchangeMode: "incremental",
      baseRevision: "accepted",
      updateBase64: "update"
    }
  );

  const acknowledged = acknowledgeAuthoringOperation({
    session: beginAuthoringReplay(queued, "reordered-projection"),
    operationId: "reordered-projection",
    document: acceptedDocument,
    acceptedRevision: "next"
  });

  assert.equal(acknowledged.status, "clean");
  assert.deepEqual(acknowledged.baseline, acceptedDocument);
  assert.deepEqual(acknowledged.draft, acceptedDocument);
});

test("runtime acknowledgement is optional when projection reconciliation already consumed the operation", () => {
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental"],
    baseline: { prose: "baseline" },
    draft: { prose: "first edit" }
  });
  runtime.queue(resource, {
    operationId: "operation",
    schemaVersion: 1,
    exchangeMode: "incremental",
    baseRevision: "accepted",
    updateBase64: "update"
  });
  runtime.beginReplay(resource, "operation");
  runtime.acknowledge({
    resource,
    operationId: "operation",
    document: { prose: "first edit" },
    acceptedRevision: "next"
  });

  const acknowledged = runtime.acknowledgeIfQueued({
    resource,
    operationId: "operation",
    document: { prose: "second accepted state" },
    acceptedRevision: "later"
  });

  assert.equal(acknowledged, false);
  assert.equal(runtime.session(resource)?.queuedOperations.length, 0);
});

test("a no-op modification keeps an unqueued authoring session clean", () => {
  const clean = startAuthoringSession({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental"],
    baseline: { prose: "baseline" },
    draft: { prose: "baseline" }
  });

  const unchanged = modifyAuthoringSession(clean, { prose: "baseline" });

  assert.equal(unchanged.status, "clean");
  assert.deepEqual(unchanged.draft, unchanged.baseline);
});

test("a settled operation is not rejected again by an async completion race", () => {
  const runtime = new AuthoringRuntime({
    sessions: {
      "Note:note": queuedSession()
    }
  });
  runtime.acknowledge({
    resource,
    operationId: "operation",
    document: { prose: "first edit" },
    acceptedRevision: "next"
  });

  const rejected = runtime.rejectIfQueued({
    resource,
    operationId: "operation",
    category: "save_rejected",
    message: "Late rejection",
    retryable: false
  });

  assert.equal(rejected, false);
  assert.equal(runtime.session(resource)?.status, "clean");
});

test("duplicate queueing and duplicate acknowledgement are idempotent", () => {
  const first = queuedSession();
  const duplicate = queueAuthoringOperation(first, {
    operationId: "operation",
    schemaVersion: 1,
    exchangeMode: "incremental",
    baseRevision: "accepted",
    updateBase64: "update"
  });
  assert.equal(duplicate.queuedOperations.length, first.queuedOperations.length);

  const accepted = acknowledgeAuthoringOperation({
    session: first,
    operationId: "operation",
    document: { prose: "first edit" },
    acceptedRevision: "next"
  });
  const duplicateAcknowledgement = acknowledgeAuthoringOperation({
    session: accepted,
    operationId: "operation",
    document: { prose: "first edit" },
    acceptedRevision: "next"
  });
  assert.equal(duplicateAcknowledgement, accepted);
});

test("restart turns an interrupted send back into a replayable durable operation", () => {
  const runtime = new AuthoringRuntime({
    sessions: {
      "Note:note": beginAuthoringReplay(queuedSession(), "operation")
    }
  });
  const restarted = new AuthoringRuntime(runtime.getSnapshot());

  const restored = restarted.session<{ prose: string }>(resource);
  assert.ok(restored);
  assert.equal(restored.status, "offlineModified");
  assert.equal(restored.queuedOperations[0]?.state, "queued");

  const connected = reconnectAuthoringSession(restored);
  assert.equal(connected.status, "replaying");
});

test("blocked operations do not replay until an explicit resolution supersedes them", () => {
  const rejected = rejectAuthoringOperation({
    session: queuedSession(),
    operationId: "operation",
    category: "conflict",
    message: "The accepted revision changed.",
    retryable: false
  });
  const offline = disconnectAuthoringSession(rejected);
  const reconnected = reconnectAuthoringSession(offline);

  assert.equal(reconnected.status, "commitRejected");
  assert.throws(
    () => beginAuthoringReplay(reconnected, "operation"),
    /while it is blocked/
  );

  const superseded = supersedeBlockedAuthoringOperations(reconnected);
  assert.equal(superseded.status, "modified");
  assert.equal(superseded.queuedOperations.length, 0);
});

test("a session accepts a draft exactly when the modify transition takes one", () => {
  for (const status of EVERY_STATUS) {
    const session = { ...modifiedSession(), status };
    let modified = true;
    try {
      modifyAuthoringSession(session, { prose: "next edit" });
    } catch {
      modified = false;
    }
    assert.equal(authoringSessionAcceptsDraft(session), modified, status);
  }
});

test("a session blocked for migration, a dependency, a resync or recovery keeps its draft", () => {
  for (const block of [
    "migrationBlocked",
    "dependencyBlocked",
    "resyncRequired",
    "recoveryRequired"
  ] as const) {
    const blocked = blockAuthoringSession(queuedSession(), block);
    assert.equal(authoringSessionAcceptsDraft(blocked), false, block);
    assert.throws(() => modifyAuthoringSession(blocked, { prose: "next edit" }), Error, block);
  }
});

test("leave decisions distinguish durable restoration from irreversible loss", () => {
  const modified = modifiedSession();
  assert.deepEqual(authoringLeaveDecision(modified, true), {
    kind: "allow",
    restoration: "durable"
  });
  assert.deepEqual(authoringLeaveDecision(modified, false), {
    kind: "confirmDiscard"
  });
  assert.equal(disconnectAuthoringSession(modified).status, "offlineModified");
});

test("authoritative baseline adoption replaces clean drafts with the new baseline", () => {
  const clean = startAuthoringSession({
    resource,
    policy: "optimisticDocument",
    schemaVersion: 1,
    acceptedRevision: "before",
    supportedExchangeModes: ["optimisticDocument"],
    baseline: { prose: "before" },
    draft: { prose: "before" }
  });
  const nextBaseline = { prose: "after external save" };

  assert.deepEqual(
    resolveAuthoringDraftForBaselineAdoption(clean, nextBaseline),
    nextBaseline
  );

  const adopted = adoptAuthoringBaseline({
    session: clean,
    baseline: nextBaseline,
    acceptedRevision: "after"
  });
  assert.deepEqual(adopted.baseline, nextBaseline);
  assert.deepEqual(adopted.draft, nextBaseline);
  assert.equal(adopted.status, "clean");
});

test("authoritative baseline adoption preserves dirty drafts", () => {
  const modified = modifiedSession();
  const nextBaseline = { prose: "external save" };

  assert.deepEqual(
    resolveAuthoringDraftForBaselineAdoption(modified, nextBaseline),
    { prose: "first edit" }
  );

  const adopted = adoptAuthoringBaseline({
    session: modified,
    baseline: nextBaseline,
    acceptedRevision: "after"
  });
  assert.deepEqual(adopted.baseline, nextBaseline);
  assert.deepEqual(adopted.draft, { prose: "first edit" });
  assert.equal(adopted.status, "modified");
});

test("authoring diagnostics distinguish blocked offline work without exposing content", () => {
  const diagnosticResource = {
    entity: "Note",
    resourceKey: "diagnostic-note"
  };
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource: diagnosticResource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "sensitive-frontier",
    supportedExchangeModes: ["incremental"],
    baseline: { prose: "private baseline" },
    draft: { prose: "private draft" },
    validation: { hidden: "private validation" }
  });
  runtime.queue(diagnosticResource, {
    operationId: "diagnostic-operation",
    schemaVersion: 1,
    exchangeMode: "incremental",
    baseRevision: "sensitive-frontier",
    updateBase64: "sensitive-update"
  });
  runtime.reject({
    resource: diagnosticResource,
    operationId: "diagnostic-operation",
    category: "dependencyBlocked",
    message: "private rejection detail",
    retryable: false
  });

  const diagnostic = redactedAuthoringRuntimeDiagnostic(runtime);
  assert.equal(diagnostic.sessionCount, 1);
  assert.equal(diagnostic.queuedOperationCount, 1);
  assert.equal(diagnostic.blockedOperationCount, 1);
  assert.deepEqual(
    diagnostic.sessions[0]?.rejectionCategories,
    ["dependencyBlocked"]
  );
  const serialized = JSON.stringify(diagnostic);
  for (const privateValue of [
    "private baseline",
    "private draft",
    "private validation",
    "sensitive-frontier",
    "sensitive-update",
    "private rejection detail"
  ]) {
    assert.equal(serialized.includes(privateValue), false);
  }
});

test("an edit made while the server is away can still be queued for replay", () => {
  // The precondition an editor's save depends on offline. A session the
  // disconnect left readable cannot carry an operation, so the save must push
  // its draft into the session first; that transition is what turns a lost
  // offline edit into queued work the reconnect replays.
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental", "bootstrap"],
    baseline: { prose: "baseline" },
    draft: { prose: "baseline" }
  });
  assert.equal(runtime.session(resource)?.status, "clean");

  runtime.disconnect(resource);
  assert.equal(runtime.session(resource)?.status, "disconnectedReadable");

  runtime.modify(resource, { prose: "written while offline" });
  assert.equal(
    runtime.session(resource)?.status,
    "offlineModified",
    "an offline edit must leave the session able to queue"
  );

  runtime.queue(resource, {
    operationId: "offline-operation",
    schemaVersion: 1,
    exchangeMode: "incremental",
    baseRevision: "accepted",
    document: { prose: "written while offline" }
  });
  const queued = runtime.session(resource)?.queuedOperations ?? [];
  assert.equal(queued.length, 1);
  assert.equal(queued[0]?.state, "queued");

  // And the reconnect replays it rather than dropping it.
  runtime.reconnect(resource);
  assert.equal(runtime.session(resource)?.status, "replaying");
});

test("a transition leaves every other session's object, and the fields it did not touch, in place", () => {
  const runtime = new AuthoringRuntime();
  const other = { entity: "Note", resourceKey: "other" } as const;
  for (const target of [resource, other]) {
    runtime.open({
      resource: target,
      policy: "collaborative",
      schemaVersion: 1,
      acceptedRevision: "accepted",
      supportedExchangeModes: ["incremental", "bootstrap"],
      baseline: { prose: "baseline" },
      draft: { prose: "baseline" }
    });
  }
  const untouchedBefore = runtime.session(other);
  const editedBefore = runtime.session(resource);
  assert.ok(untouchedBefore && editedBefore);

  runtime.modify(resource, { prose: "edit" });

  const editedAfter = runtime.session(resource);
  assert.ok(editedAfter);
  assert.equal(runtime.session(other), untouchedBefore);
  assert.equal(editedAfter.baseline, editedBefore.baseline);
  assert.notEqual(editedAfter.draft, editedBefore.draft);
});

test("a batch notifies subscribers once, after its last transition, and only when something changed", () => {
  const runtime = new AuthoringRuntime();
  let notifications = 0;
  runtime.subscribe(() => {
    notifications += 1;
  });

  runtime.batch(() => {
    for (const key of ["a", "b", "c"]) {
      runtime.open({
        resource: { entity: "Note", resourceKey: key },
        policy: "collaborative",
        schemaVersion: 1,
        acceptedRevision: "accepted",
        supportedExchangeModes: ["incremental", "bootstrap"],
        baseline: { prose: key },
        draft: { prose: key }
      });
      assert.equal(notifications, 0);
    }
    runtime.batch(() => {
      runtime.modify({ entity: "Note", resourceKey: "a" }, { prose: "edited" });
    });
    assert.equal(notifications, 0);
  });
  assert.equal(notifications, 1);
  assert.equal(Object.keys(runtime.getSnapshot().sessions).length, 3);

  runtime.batch(() => undefined);
  assert.equal(notifications, 1);

  runtime.modify({ entity: "Note", resourceKey: "b" }, { prose: "outside a batch" });
  assert.equal(notifications, 2);
});

test("a live session imports accepted operations before reconciling the retained draft", () => {
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "before",
    supportedExchangeModes: ["incremental"],
    baseline: { prose: "before" },
    draft: { prose: "local" }
  });
  const operations: string[] = [];
  const session = runtime.ensureController<{ prose: string }, "prose">(
    resource,
    () => ({
      importUpdateBase64: () => { operations.push("import"); },
      replaceDraft: () => { operations.push("replace"); }
    })
  );
  operations.length = 0;

  session.adoptAccepted({
    updateBase64: "accepted-update",
    acceptedFrontierBase64: "after",
    baseline: { prose: "accepted" },
    draft: { prose: "local" },
    acceptedRevision: "after"
  });

  assert.deepEqual(operations, ["import", "replace"]);
  assert.deepEqual(runtime.session(resource)?.baseline, { prose: "accepted" });
  assert.deepEqual(runtime.session(resource)?.draft, { prose: "local" });
  assert.equal(runtime.session(resource)?.acceptedRevision, "after");
});
