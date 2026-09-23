/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Editor-facing text transactions. Positions are UTF-16 offsets in the base view.
 */
export interface TextEdit {
  readonly from: number;
  readonly to: number;
  readonly insert: string;
}

export interface TextSelection {
  readonly anchor: number;
  readonly head: number;
  readonly affinity?: -1 | 1;
}

export interface TextBindingChange {
  readonly revision: number;
  readonly changes: readonly TextEdit[];
  readonly origin: "local" | "remote" | "history";
  readonly available: boolean;
}

export interface TextBinding {
  readonly revision: number;
  readonly available: boolean;
  /** Initial attachment and explicit capture only; edits are delivered as ranges. */
  read(): string;
  subscribe(listener: (change: TextBindingChange) => void): () => void;
  edit(input: {
    baseRevision: number;
    changes: readonly TextEdit[];
    selectionBefore: TextSelection;
    selectionAfter: TextSelection;
    /** Consecutive typing shares a group; paste/composition use distinct groups. */
    group: string;
  }): void;
  undo(selection: TextSelection): TextSelection | null;
  redo(selection: TextSelection): TextSelection | null;
  /** Imports wait until the composing view has committed or cancelled its edit. */
  beginComposition(): () => void;
}

/** Validate the complete transaction before mutating the replica. */
export function validateTextEdits(
  length: number,
  changes: readonly TextEdit[],
  selectionBefore: TextSelection,
  selectionAfter: TextSelection
): void {
  let previousEnd = 0;
  let nextLength = length;
  for (const change of changes) {
    if (!Number.isInteger(change.from) || !Number.isInteger(change.to)
      || change.from < previousEnd || change.to < change.from || change.to > length) {
      throw new Error("Text edits must be ordered, non-overlapping ranges in the base view.");
    }
    previousEnd = change.to;
    nextLength += change.insert.length - (change.to - change.from);
  }
  for (const [selection, size] of [[selectionBefore, length], [selectionAfter, nextLength]] as const) {
    if (![selection.anchor, selection.head].every((offset) =>
      Number.isInteger(offset) && offset >= 0 && offset <= size)) {
      throw new Error("Text selection is outside the document.");
    }
  }
}
