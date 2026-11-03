/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A text binding's undo and redo stacks. Each item restores a selection held as UTF-16
 * offsets in the text before the item's change; undoing the item rebases the offsets over
 * the remote changes made since, as Loro's undo manager rebases its items' cursors.
 */

/** A text change as lengths in UTF-16 code units. */
export type TextDeltaOp =
  | { readonly retain: number }
  | { readonly insert: number }
  | { readonly delete: number };
export type TextDelta = readonly TextDeltaOp[];

type OpKind = "retain" | "insert" | "delete";

function opKind(op: TextDeltaOp): OpKind {
  return "retain" in op ? "retain" : "insert" in op ? "insert" : "delete";
}

function opLength(op: TextDeltaOp): number {
  return "retain" in op ? op.retain : "insert" in op ? op.insert : op.delete;
}

/** Walks a delta's operations; past the end it reads as an unbounded retain. */
class DeltaReader {
  private index = 0;
  private offset = 0;

  constructor(private readonly ops: TextDelta) {}

  more(): boolean { return this.index < this.ops.length; }
  kind(): OpKind {
    const op = this.ops[this.index];
    return op === undefined ? "retain" : opKind(op);
  }
  length(): number {
    const op = this.ops[this.index];
    return op === undefined ? Infinity : opLength(op) - this.offset;
  }
  /** Consumes `length` units of the current operation, or all of it. */
  take(length = this.length()): number {
    const op = this.ops[this.index];
    if (op === undefined) return length;
    if (this.offset + length >= opLength(op)) {
      this.index += 1;
      this.offset = 0;
    } else {
      this.offset += length;
    }
    return length;
  }
}

/** Builds a delta with adjacent operations of a kind merged and a deletion before an insertion at one position. */
class DeltaWriter {
  private readonly ops: { kind: OpKind; length: number }[] = [];

  retain(length: number): void { this.push("retain", length); }
  insert(length: number): void { this.push("insert", length); }
  delete(length: number): void {
    if (length <= 0) return;
    const last = this.ops.at(-1);
    if (last?.kind !== "insert") {
      this.push("delete", length);
      return;
    }
    const beforeLast = this.ops.at(-2);
    if (beforeLast?.kind === "delete") beforeLast.length += length;
    else this.ops.splice(this.ops.length - 1, 0, { kind: "delete", length });
  }
  done(): TextDelta {
    if (this.ops.at(-1)?.kind === "retain") this.ops.pop();
    return this.ops.map(({ kind, length }) =>
      kind === "retain" ? { retain: length } : kind === "insert" ? { insert: length } : { delete: length });
  }
  private push(kind: OpKind, length: number): void {
    if (length <= 0) return;
    const last = this.ops.at(-1);
    if (last?.kind === kind) last.length += length;
    else this.ops.push({ kind, length });
  }
}

/** The change of applying `first`, then `second`. */
export function composeDeltas(first: TextDelta, second: TextDelta): TextDelta {
  const a = new DeltaReader(first);
  const b = new DeltaReader(second);
  const out = new DeltaWriter();
  while (a.more() || b.more()) {
    if (b.kind() === "insert") {
      out.insert(b.take());
    } else if (a.kind() === "delete") {
      out.delete(a.take());
    } else {
      const length = Math.min(a.length(), b.length());
      const aKind = a.kind();
      const bKind = b.kind();
      a.take(length);
      b.take(length);
      if (bKind === "retain") {
        if (aKind === "insert") out.insert(length);
        else out.retain(length);
      } else if (aKind === "retain") {
        out.delete(length);
      }
    }
  }
  return out.done();
}

/**
 * `delta` rebased to apply after `other`, both made from one text. At one position, `other`'s
 * insertion comes first unless `deltaFirst`.
 */
export function transformDelta(delta: TextDelta, other: TextDelta, deltaFirst: boolean): TextDelta {
  const a = new DeltaReader(delta);
  const b = new DeltaReader(other);
  const out = new DeltaWriter();
  while (a.more() || b.more()) {
    if (a.kind() === "insert" && (deltaFirst || b.kind() !== "insert")) {
      out.insert(a.take());
    } else if (b.kind() === "insert") {
      out.retain(b.take());
    } else {
      const length = Math.min(a.length(), b.length());
      const aKind = a.kind();
      const bKind = b.kind();
      a.take(length);
      b.take(length);
      if (bKind === "delete") continue;
      if (aKind === "delete") out.delete(length);
      else out.retain(length);
    }
  }
  return out.done();
}

/** Where `position` moves under `delta`. An insertion at `position` leaves it before the inserted text. */
export function transformPosition(delta: TextDelta, position: number): number {
  let index = 0;
  for (const op of delta) {
    if ("retain" in op) {
      index += op.retain;
      if (index >= position) return position;
    } else if ("insert" in op) {
      if (index === position) return position;
      position += op.insert;
      index += op.insert;
    } else {
      position = Math.max(0, position - op.delete);
      if (position < index) return index;
    }
  }
  return position;
}

/** The change that reverts `delta`, made from the text `delta` produced. */
export function invertDelta(delta: TextDelta): TextDelta {
  const out = new DeltaWriter();
  for (const op of delta) {
    if ("retain" in op) out.retain(op.retain);
    else if ("insert" in op) out.delete(op.insert);
    else out.insert(op.delete);
  }
  return out.done();
}

/** An offset in the text before an item's change, or the end of the text, which stays the end. */
export type HistoryOffset = number | "end";

/** The selection an item restores. */
export interface HistorySelection {
  readonly anchor: HistoryOffset;
  readonly head: HistoryOffset;
  readonly affinity: -1 | 1;
}

/** An undo or redo item: the counter range of its local operations, its change, and the selection it restores. */
export interface HistoryItem {
  readonly start: number;
  end: number;
  change: TextDelta;
  readonly selection: HistorySelection;
}

/** Items pushed between two remote changes share a row with the remote changes made after them. */
interface HistoryRow {
  readonly items: HistoryItem[];
  remote: TextDelta;
}

/** One of Loro's undo manager's stacks, followed through its pushes, pops and evictions. */
export class HistoryStack {
  private rows: HistoryRow[] = [{ items: [], remote: [] }];
  private size = 0;

  get length(): number { return this.size; }

  /** The top item, when no remote change followed it. */
  extendable(): HistoryItem | undefined {
    const row = this.rows.at(-1)!;
    return row.remote.length === 0 ? row.items.at(-1) : undefined;
  }

  push(item: HistoryItem): void {
    const row = this.rows.at(-1)!;
    if (row.remote.length === 0) row.items.push(item);
    else this.rows.push({ items: [item], remote: [] });
    this.size += 1;
  }

  /** Records a remote change made after every item. */
  receiveRemote(change: TextDelta): void {
    if (this.size === 0 || change.length === 0) return;
    const row = this.rows.at(-1)!;
    row.remote = composeDeltas(row.remote, change);
  }

  /**
   * Removes the top item and returns it with the selection it restores once its change is
   * reverted: its offsets moved over the remote changes made since, rebased to follow the revert.
   */
  pop(): { item: HistoryItem; selection: HistorySelection } | undefined {
    while (this.rows.length > 1 && this.rows.at(-1)!.items.length === 0) {
      const emptied = this.rows.pop()!;
      const below = this.rows.at(-1)!;
      below.remote = composeDeltas(below.remote, emptied.remote);
    }
    const row = this.rows.at(-1)!;
    const item = row.items.pop();
    if (item === undefined) {
      row.remote = [];
      return undefined;
    }
    this.size -= 1;
    row.remote = transformDelta(row.remote, invertDelta(item.change), false);
    const { remote } = row;
    const move = (offset: HistoryOffset) => offset === "end" ? offset : transformPosition(remote, offset);
    const selection = { anchor: move(item.selection.anchor), head: move(item.selection.head), affinity: item.selection.affinity };
    if (this.size === 0) this.clear();
    return { item, selection };
  }

  /** Removes the oldest item. */
  evictOldest(): void {
    if (this.size === 0) return;
    while (this.rows[0]!.items.length === 0) this.rows.shift();
    this.rows[0]!.items.shift();
    this.size -= 1;
    if (this.rows[0]!.items.length === 0) this.rows.shift();
    if (this.rows.length === 0) this.rows.push({ items: [], remote: [] });
  }

  clear(): void {
    this.rows = [{ items: [], remote: [] }];
    this.size = 0;
  }
}
