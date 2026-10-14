/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Timers a test advances by hand: nothing scheduled runs until the test runs it.
 */
import type { Timers } from "@clerkenwell/client";

export class ManualTimers implements Timers {
  private time = 0;
  private nextHandle = 0;
  private readonly scheduled = new Map<number, { readonly at: number; readonly callback: () => void }>();

  schedule(callback: () => void, delayMs: number): number {
    const handle = this.nextHandle++;
    this.scheduled.set(handle, { at: this.time + delayMs, callback });
    return handle;
  }

  cancel(handle: unknown): void {
    this.scheduled.delete(handle as number);
  }

  now(): number {
    return this.time;
  }

  /** How long from now each scheduled callback runs, soonest first. */
  pendingDelays(): number[] {
    return [...this.scheduled.values()].map(({ at }) => at - this.time).sort((left, right) => left - right);
  }

  /** Moves time to the soonest scheduled callback and runs it. */
  runNext(): void {
    const next = [...this.scheduled.entries()].sort(([, left], [, right]) => left.at - right.at)[0];
    if (!next) throw new Error("No callback is scheduled.");
    const [handle, { at, callback }] = next;
    this.scheduled.delete(handle);
    this.time = at;
    callback();
  }
}
