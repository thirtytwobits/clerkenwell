/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Text-binding helpers: an isolated one-field replica and single-range edits.
 */
import type { CollaborationEntityPlan, TextBinding } from "@clerkenwell/client";
import { CollaborationReplica } from "@clerkenwell/client/replica";

const SCRATCH_PLAN = {
  schemaVersion: 1,
  migrationIds: [],
  authoringState: { projection: "scratch.authoringState", importMutation: "scratch.importUpdate" },
  rootContainer: "scratch",
  fields: {
    text: {
      path: "text",
      storage: { kind: "text", container: "text" },
      value: { codec: "string" },
      required: true,
      requiredInParent: true,
      conflict: "merge"
    }
  }
} as const satisfies CollaborationEntityPlan;

/** A text field with no transport or owner behind it. */
export function createScratchTextBinding(text: string): TextBinding {
  return CollaborationReplica.from("Scratch", SCRATCH_PLAN, {
    kind: "document",
    document: { text }
  }).bindText("text");
}

export function insert(binding: TextBinding, at: number, text: string, group = "typing"): void {
  binding.edit({
    baseRevision: binding.revision,
    changes: [{ from: at, to: at, insert: text }],
    selectionBefore: { anchor: at, head: at },
    selectionAfter: { anchor: at + text.length, head: at + text.length },
    group
  });
}

/** Replace the whole text in one transaction. */
export function replaceAll(binding: TextBinding, text: string, group = "replace"): void {
  const current = binding.read();
  binding.edit({
    baseRevision: binding.revision,
    changes: [{ from: 0, to: current.length, insert: text }],
    selectionBefore: { anchor: current.length, head: current.length },
    selectionAfter: { anchor: text.length, head: text.length },
    group
  });
}
