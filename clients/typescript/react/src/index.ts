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
} from "./authoring-runtime";
export type {
  CollaborativeAutosyncHandle,
  CollaborativeAutosyncOptions
} from "./collaborative-autosync";
export {
  DEFAULT_COLLABORATIVE_AUTOSYNC_DELAY_MS,
  useCollaborativeAutosync
} from "./collaborative-autosync";
export { useProjectionSelector, useProjectionSnapshot } from "./hooks";
export type {
  ProjectionEditOverlayOptions,
  ProjectionEditOverlaySnapshot,
  ProjectionOverlayConflict,
  ProjectionOverlayStatus
} from "./overlay";
export { ProjectionEditOverlay } from "./overlay";
export type { ProjectionReactStoreListener, ProjectionReadableStore } from "./store";
export { ProjectionExternalStore } from "./store";
export { replaceTextBindingValue, useTextBindingValue } from "./text-binding";
