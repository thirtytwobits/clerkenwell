/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Exercises the shared collaborative autosync trigger contract.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  rejectedCollaborativeAutosyncKeyAfterDraftChange,
  shouldScheduleCollaborativeAutosync
} from "@clerkenwell/react";

const READY_DIRTY_DRAFT = {
  draftKey: "draft-a",
  enabled: true,
  hasDraft: true,
  isBlocked: false,
  isDirty: true,
  isSyncing: false,
  lastRejectedDraftKey: null
};

test("collaborative autosync schedules only for an enabled dirty draft", () => {
  assert.equal(shouldScheduleCollaborativeAutosync(READY_DIRTY_DRAFT), true);
  assert.equal(shouldScheduleCollaborativeAutosync({
    ...READY_DIRTY_DRAFT,
    isDirty: false
  }), false);
  assert.equal(shouldScheduleCollaborativeAutosync({
    ...READY_DIRTY_DRAFT,
    isBlocked: true
  }), false);
  assert.equal(shouldScheduleCollaborativeAutosync({
    ...READY_DIRTY_DRAFT,
    isSyncing: true
  }), false);
  assert.equal(shouldScheduleCollaborativeAutosync({
    ...READY_DIRTY_DRAFT,
    hasDraft: false,
    draftKey: null
  }), false);
});

test("collaborative autosync does not reschedule the same rejected draft", () => {
  assert.equal(shouldScheduleCollaborativeAutosync({
    ...READY_DIRTY_DRAFT,
    lastRejectedDraftKey: "draft-a"
  }), false);
  assert.equal(shouldScheduleCollaborativeAutosync({
    ...READY_DIRTY_DRAFT,
    draftKey: "draft-b",
    lastRejectedDraftKey: "draft-a"
  }), true);
});

test("collaborative autosync clears rejected draft suppression after an edit", () => {
  assert.equal(
    rejectedCollaborativeAutosyncKeyAfterDraftChange({
      currentDraftKey: "draft-a",
      lastRejectedDraftKey: "draft-a",
      lastSeenDraftKey: "draft-a"
    }),
    "draft-a"
  );
  assert.equal(
    rejectedCollaborativeAutosyncKeyAfterDraftChange({
      currentDraftKey: "draft-b",
      lastRejectedDraftKey: "draft-a",
      lastSeenDraftKey: "draft-a"
    }),
    null
  );
});
