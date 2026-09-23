/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Container templates resolve against the identities of the enclosing items.
 */
import assert from "node:assert/strict";
import test from "node:test";

import { resolveCollaborationContainer } from "@clerkenwell/client";

import { BOARD_PLAN } from "./support/plans";

test("a container template takes each placeholder from the identities it is given", () => {
  const identities = { column_id: "todo", task_id: "task-1" };
  const template = BOARD_PLAN.fields["columns.*.tasks.*.notes"].storage.containerTemplate;

  const container = resolveCollaborationContainer(template, identities);

  assert.equal(
    container,
    template.replace("{column_id}", identities.column_id).replace("{task_id}", identities.task_id)
  );
});

test("a template without placeholders names its container directly", () => {
  const container = BOARD_PLAN.fields.columns.storage.orderContainer;

  assert.equal(resolveCollaborationContainer(container, {}), container);
});

test("a missing or empty identity refuses to resolve", () => {
  const template = BOARD_PLAN.fields["columns.*.tasks.*.notes"].storage.containerTemplate;

  assert.throws(() => resolveCollaborationContainer(template, { column_id: "todo" }), /task_id/);
  assert.throws(
    () => resolveCollaborationContainer(template, { column_id: "", task_id: "task-1" }),
    /column_id/
  );
});
