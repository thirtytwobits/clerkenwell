/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * React reads a text binding without owning its text, and replaces a staged
 * binding's text in one transaction.
 */
import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

import type { CollaborationEntityPlan, TextBinding, TextBindingChange } from "@clerkenwell/client";
import { CollaborationLoroAuthoringDocument } from "@clerkenwell/client/loro";
import { replaceTextBindingValue, useTextBindingValue } from "@clerkenwell/react";

const PLAN = {
  substrate: "loro",
  schemaVersion: 1,
  migrationIds: [],
  authoringState: { projection: "memo.authoringState", importMutation: "memo.importUpdate" },
  rootContainer: "memo",
  clientPathNaming: "camelCase",
  fields: {
    text: {
      path: "text",
      storage: { kind: "text", container: "text" },
      value: { codec: "string" },
      required: true,
      conflict: "merge"
    }
  }
} as const satisfies CollaborationEntityPlan;

function binding(text: string): TextBinding {
  return new CollaborationLoroAuthoringDocument("Memo", PLAN, { kind: "document", document: { text } })
    .bindText("text");
}

function Reader({ source }: { source: TextBinding | null }): React.ReactElement {
  return <output>{useTextBindingValue(source)}</output>;
}

test("a component reads a binding's current text", () => {
  const text = "Bound text";
  const source = binding(text);

  assert.equal(renderToStaticMarkup(<Reader source={source} />), `<output>${text}</output>`);

  const replacement = "Edited text";
  replaceTextBindingValue(source, replacement);
  assert.equal(renderToStaticMarkup(<Reader source={source} />), `<output>${replacement}</output>`);
});

test("a component without a binding reads empty text", () => {
  assert.equal(renderToStaticMarkup(<Reader source={null} />), "<output></output>");
});

test("replacing a binding's text is one edit, and replacing it with itself is none", () => {
  const source = binding("Draft");
  const changes: TextBindingChange[] = [];
  source.subscribe((change) => changes.push(change));
  const replacement = "Final";

  replaceTextBindingValue(source, replacement);
  replaceTextBindingValue(source, replacement);

  assert.equal(source.read(), replacement);
  assert.equal(changes.length, 1);
  assert.equal(changes[0]?.origin, "local");
});
