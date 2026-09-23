/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A projection model and composition plans with one projection per
 * materialisation strategy.
 */
import type { ProjectionCompositionPlans } from "@clerkenwell/client";

export interface NoteSummary {
  note_id: string;
  title: string;
}

export interface NoteRecord {
  title: string;
  body: string;
}

export interface RunRecord {
  run_id: string;
  output?: { sequence: number; text: string };
}

export type TestProjectionModel = {
  projections: {
    "notes.list": {
      params: Record<string, never>;
      snapshot: { notes: NoteSummary[] };
      patch:
        | { kind: "reset"; notes: NoteSummary[] }
        | { kind: "upsert"; summary: NoteSummary }
        | { kind: "remove"; note_id: string };
    };
    "notes.byId": {
      params: { note_id: string };
      snapshot: { note_id?: string; note: NoteRecord | null };
      patch:
        | { kind: "replace"; note_id: string; note: NoteRecord }
        | { kind: "update"; changes: Partial<NoteRecord> }
        | { kind: "remove"; note_id: string };
    };
    "notes.authoringState": {
      params: { note_id: string };
      snapshot: { update_base64: string } | null;
      patch: { kind: "replace"; state: { update_base64: string } } | { kind: "remove" };
    };
    "boards.summary": {
      params: Record<string, never>;
      snapshot: { count: number };
      patch: { kind: "replace"; count: number };
    };
    "boards.detail": {
      params: { board_id: string };
      snapshot: { title: string };
      patch: { kind: "replace"; board: { title: string } };
    };
    "activity.log": {
      params: Record<string, never>;
      snapshot: { entries: string[] };
      patch: { kind: "reset"; entries: string[] };
    };
    "runs.transcript": {
      params: Record<string, never>;
      snapshot: { runs: RunRecord[] };
      patch:
        | { kind: "reset"; runs: RunRecord[] }
        | { kind: "output"; run_id: string; delta: { sequence: number; text: string } };
    };
  };
  mutations: {
    "note.rename": {
      params: { note_id: string; title: string };
      result: { note_id: string; title: string };
    };
  };
};

export const TEST_COMPOSITION_PLANS = {
  "notes.list": {
    strategy: "keyedCollection",
    collectionField: "notes",
    itemField: "summary",
    itemIdentityField: "note_id",
    patchIdentityField: "note_id",
    dependsOn: ["Note"]
  },
  "notes.byId": {
    strategy: "replaceOrRemove",
    updateField: "note",
    updatesField: "changes",
    snapshotMode: "patch",
    snapshotOmitFields: ["note_id"],
    removeMode: "nullField",
    removeField: "note",
    dependsOn: ["Note"]
  },
  "notes.authoringState": {
    strategy: "replaceOrRemove",
    snapshotMode: "field",
    snapshotField: "state",
    removeMode: "nullSnapshot",
    dependsOn: ["Note"]
  },
  "boards.summary": {
    strategy: "replace",
    snapshotMode: "patch",
    dependsOn: ["Board"]
  },
  "boards.detail": {
    strategy: "replace",
    snapshotMode: "field",
    snapshotField: "board",
    dependsOn: ["Board"]
  },
  "activity.log": {
    strategy: "reset",
    dependsOn: ["Note", "Board"]
  },
  "runs.transcript": {
    strategy: "sequencedText",
    collectionField: "runs",
    itemIdentityField: "run_id",
    patchIdentityField: "run_id",
    snapshotOutputField: "output",
    sequenceField: "sequence",
    textField: "text",
    patchOutputField: "delta",
    deltaTextField: "text",
    dependsOn: ["Run"]
  }
} as const satisfies ProjectionCompositionPlans<
  keyof TestProjectionModel["projections"],
  "Note" | "Board" | "Run"
>;
