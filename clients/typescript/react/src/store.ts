/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Generic external-store primitive for React projection adapters.
 */

export type ProjectionReactStoreListener = () => void;

export interface ProjectionReadableStore<TSnapshot> {
  subscribe(listener: ProjectionReactStoreListener): () => void;
  getSnapshot(): TSnapshot;
}

export class ProjectionExternalStore<TSnapshot>
implements ProjectionReadableStore<TSnapshot> {
  private readonly listeners = new Set<ProjectionReactStoreListener>();

  constructor(private snapshot: TSnapshot) {}

  subscribe = (listener: ProjectionReactStoreListener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getSnapshot = (): TSnapshot => this.snapshot;

  setSnapshot(snapshot: TSnapshot): void {
    if (Object.is(this.snapshot, snapshot)) {
      return;
    }
    this.snapshot = snapshot;
    this.listeners.forEach((listener) => listener());
  }

  updateSnapshot(updater: (snapshot: TSnapshot) => TSnapshot): TSnapshot {
    const snapshot = updater(this.snapshot);
    this.setSnapshot(snapshot);
    return snapshot;
  }

  dispose(): void {
    this.listeners.clear();
  }
}
