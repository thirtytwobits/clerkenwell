/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The timers and clock the client waits by. An application passes its own to
 * decide when a socket reopens and when a wait for a snapshot gives up.
 */

/** Schedules work for later and reads the time it is scheduled against. */
export interface Timers {
  /** Runs `callback` once, `delayMs` milliseconds from now. */
  schedule(callback: () => void, delayMs: number): unknown;
  /** Stops a scheduled callback from running; one that ran already is ignored. */
  cancel(handle: unknown): void;
  /** The time now, in epoch milliseconds. */
  now(): number;
}

/** The platform's timers and clock. */
export const PLATFORM_TIMERS: Timers = {
  schedule: (callback, delayMs) => setTimeout(callback, delayMs),
  cancel: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  now: () => Date.now()
};
