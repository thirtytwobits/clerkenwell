/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Generated bindings name the client's authoring-state types, and every
 * authoring-state projection delivers them.
 */
import assert from "node:assert/strict";
import test from "node:test";

import type {
  AuthoringExchangeMode,
  AuthoringPeerBlock,
  AuthoringState,
  AuthoringStatePatch,
  CollaborationExchangeMode
} from "@clerkenwell/client";

import type * as Notebook from "../../../../crates/clerkenwell-notebook/generated/typescript/index.js";

/** `true` exactly when `A` and `B` are one type. */
type Same<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;

test("the generated bindings' authoring-state types are the client's", () => {
  const identities: readonly true[] = [
    true satisfies Same<Notebook.AuthoringState, AuthoringState>,
    true satisfies Same<Notebook.AuthoringStatePatch, AuthoringStatePatch>,
    true satisfies Same<Notebook.AuthoringPeerBlock, AuthoringPeerBlock>,
    true satisfies Same<Notebook.CollaborationExchangeMode, CollaborationExchangeMode>,
    true satisfies Same<Notebook.ProjectionSnapshotByName["notes.authoringState"], AuthoringState>,
    true satisfies Same<Notebook.ProjectionPatchByName["boards.authoringState"], AuthoringStatePatch>,
    true satisfies Same<Exclude<AuthoringExchangeMode, "optimisticDocument">, CollaborationExchangeMode>
  ];
  assert.ok(identities.every(Boolean));
});
