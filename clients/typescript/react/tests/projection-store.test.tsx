/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * External stores and the field-level edit overlay.
 */
import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

import {
  ProjectionEditOverlay,
  ProjectionExternalStore,
  useProjectionSnapshot
} from "@clerkenwell/react";

interface DemoDraft extends Record<string, unknown> {
  title: string;
  summary: string;
}

test("projection React hook reads a generic external store snapshot", () => {
  const store = new ProjectionExternalStore({
    status: "idle"
  });

  function Status(): React.ReactElement {
    const snapshot = useProjectionSnapshot(store);
    return <p>Status: {snapshot.status}</p>;
  }

  assert.match(renderToStaticMarkup(<Status />), /Status: idle/);
});

test("projection edit overlay keeps dirty fields while hydrating clean fields", () => {
  const overlay = new ProjectionEditOverlay<DemoDraft, number>({
    emptyDraft: emptyDraft()
  });

  overlay.hydrate({
    title: "Server Title",
    summary: "Server summary."
  }, 1);
  overlay.setField("title", "Local Title");
  overlay.hydrate({
    title: "Server Title",
    summary: "Remote summary."
  }, 2);

  const snapshot = overlay.getSnapshot();
  assert.equal(snapshot.status, "dirty");
  assert.equal(snapshot.revision, 2);
  assert.equal(snapshot.draft.title, "Local Title");
  assert.equal(snapshot.draft.summary, "Remote summary.");
  assert.deepEqual([...snapshot.dirtyFields], ["title"]);
});

test("projection edit overlay clears committed fields and preserves other dirty fields", () => {
  const overlay = new ProjectionEditOverlay<DemoDraft, number>({
    emptyDraft: emptyDraft()
  });

  overlay.hydrate({
    title: "Server Title",
    summary: "Server summary."
  }, 1);
  overlay.setField("title", "Local Title");
  overlay.setField("summary", "Local summary.");
  overlay.markCommitting(["title"]);
  overlay.markCommitted(["title"], {
    title: "Accepted Title",
    summary: "Remote summary."
  }, 2);

  const snapshot = overlay.getSnapshot();
  assert.equal(snapshot.status, "dirty");
  assert.equal(snapshot.revision, 2);
  assert.equal(snapshot.draft.title, "Accepted Title");
  assert.equal(snapshot.draft.summary, "Local summary.");
  assert.deepEqual([...snapshot.dirtyFields], ["summary"]);
  assert.deepEqual([...snapshot.committingFields], []);
});

test("projection edit overlay clears restored dirty fields that match hydration", () => {
  const overlay = new ProjectionEditOverlay<DemoDraft, number>({
    emptyDraft: emptyDraft()
  });

  overlay.restoreDraft({
    title: "Persisted title",
    summary: "Server summary."
  }, {
    dirtyFields: ["title", "summary"],
    revision: 1
  });
  overlay.hydrate({
    title: "Server title",
    summary: "Server summary."
  }, 2);

  const snapshot = overlay.getSnapshot();
  assert.equal(snapshot.status, "dirty");
  assert.deepEqual([...snapshot.dirtyFields], ["title"]);
  assert.equal(snapshot.draft.title, "Persisted title");
  assert.equal(snapshot.draft.summary, "Server summary.");
});

test("projection edit overlay marks dirty fields stale when authoritative values change", () => {
  const overlay = new ProjectionEditOverlay<DemoDraft, number>({
    emptyDraft: emptyDraft()
  });

  overlay.hydrate({
    title: "Server title",
    summary: "Server summary."
  }, 1);
  overlay.setField("title", "Local title");
  overlay.hydrate({
    title: "Remote title",
    summary: "Server summary."
  }, 2);

  const snapshot = overlay.getSnapshot();
  assert.equal(snapshot.status, "stale");
  assert.equal(snapshot.conflict?.kind, "stale");
  assert.deepEqual([...(snapshot.conflict?.fields ?? [])], ["title"]);
  assert.equal(snapshot.draft.title, "Local title");
  assert.equal(snapshot.authoritative?.title, "Remote title");
});

test("projection edit overlay can keep a stale local draft explicitly", () => {
  const overlay = new ProjectionEditOverlay<DemoDraft, number>({
    emptyDraft: emptyDraft()
  });

  overlay.hydrate({
    title: "Server title",
    summary: "Server summary."
  }, 1);
  overlay.setField("title", "Local title");
  overlay.hydrate({
    title: "Remote title",
    summary: "Server summary."
  }, 2);
  overlay.keepDraft(["title"]);

  const snapshot = overlay.getSnapshot();
  assert.equal(snapshot.status, "dirty");
  assert.equal(snapshot.conflict, null);
  assert.equal(snapshot.draft.title, "Local title");
  assert.deepEqual([...snapshot.dirtyFields], ["title"]);
});

test("projection edit overlay can reset stale fields to authoritative values", () => {
  const overlay = new ProjectionEditOverlay<DemoDraft, number>({
    emptyDraft: emptyDraft()
  });

  overlay.hydrate({
    title: "Server title",
    summary: "Server summary."
  }, 1);
  overlay.setField("title", "Local title");
  overlay.hydrate({
    title: "Remote title",
    summary: "Server summary."
  }, 2);
  overlay.resetFields(["title"]);

  const snapshot = overlay.getSnapshot();
  assert.equal(snapshot.status, "clean");
  assert.equal(snapshot.conflict, null);
  assert.equal(snapshot.draft.title, "Remote title");
  assert.deepEqual([...snapshot.dirtyFields], []);
});

test("projection edit overlay reports commit failures as conflicts", () => {
  const overlay = new ProjectionEditOverlay<DemoDraft, number>({
    emptyDraft: emptyDraft()
  });

  overlay.hydrate({
    title: "Server Title",
    summary: "Server summary."
  }, 1);
  overlay.setField("title", "Local Title");
  overlay.markCommitting(["title"]);
  overlay.markCommitFailed(["title"], new Error("stale title"));

  const snapshot = overlay.getSnapshot();
  assert.equal(snapshot.status, "conflicted");
  assert.equal(snapshot.conflict?.kind, "commit");
  assert.equal(snapshot.conflict?.message, "stale title");
  assert.deepEqual([...(snapshot.conflict?.fields ?? [])], ["title"]);
  assert.deepEqual([...snapshot.committingFields], []);
});

function emptyDraft(): DemoDraft {
  return {
    title: "",
    summary: ""
  };
}
