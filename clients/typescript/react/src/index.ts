/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * React primitives for projection-backed stores, edit overlays and authoring.
 * Only what this file names is public.
 */
export {
  AuthoringRuntimeProvider,
  useAuthoringRuntime,
  useAuthoringRuntimePersistence,
  useAuthoringRuntimeSnapshot
} from "./authoring-runtime.js";
export type {
  CollaborativeAutosyncHandle,
  CollaborativeAutosyncOptions
} from "./collaborative-autosync.js";
export {
  DEFAULT_COLLABORATIVE_AUTOSYNC_DELAY_MS,
  useCollaborativeAutosync
} from "./collaborative-autosync.js";
export { useProjectionSelector, useProjectionSnapshot } from "./hooks.js";
export type {
  ProjectionEditOverlayOptions,
  ProjectionEditOverlaySnapshot,
  ProjectionOverlayConflict,
  ProjectionOverlayStatus
} from "./overlay.js";
export { ProjectionEditOverlay } from "./overlay.js";
export type { ProjectionReactStoreListener, ProjectionReadableStore } from "./store.js";
export { ProjectionExternalStore } from "./store.js";
export { replaceTextBindingValue, useTextBindingValue } from "./text-binding.js";
