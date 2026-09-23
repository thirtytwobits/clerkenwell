/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Generic React hooks for projection-backed external stores.
 */
import { useSyncExternalStore } from "react";

import type { ProjectionReadableStore } from "./store";

export function useProjectionSnapshot<TSnapshot>(
  store: ProjectionReadableStore<TSnapshot>
): TSnapshot {
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

export function useProjectionSelector<TSnapshot, TSelected>(
  store: ProjectionReadableStore<TSnapshot>,
  selector: (snapshot: TSnapshot) => TSelected
): TSelected {
  return selector(useProjectionSnapshot(store));
}
