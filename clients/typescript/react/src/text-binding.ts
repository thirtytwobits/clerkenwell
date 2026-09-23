/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * React access to text bindings.
 */
import React from "react";

import type { TextBinding } from "@clerkenwell/client";

/** Read a text binding through React without turning its text into component authority. */
export function useTextBindingValue(binding: TextBinding | null): string {
  const subscribe = React.useCallback((notify: () => void) => (
    binding?.subscribe(() => notify()) ?? (() => undefined)
  ), [binding]);
  const getSnapshot = React.useCallback(() => binding?.revision ?? -1, [binding]);
  React.useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return binding?.read() ?? "";
}

/** Replace a private staged binding through one editor transaction. */
export function replaceTextBindingValue(
  binding: TextBinding,
  value: string,
  group: string = "staged-edit"
): void {
  const current = binding.read();
  if (current === value) return;
  binding.edit({
    baseRevision: binding.revision,
    changes: [{ from: 0, to: current.length, insert: value }],
    selectionBefore: { anchor: current.length, head: current.length },
    selectionAfter: { anchor: value.length, head: value.length },
    group
  });
}
