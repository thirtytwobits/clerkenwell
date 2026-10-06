/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The vocabulary of authoring: a session's state, its queued operations, and
 * the controllers an application edits a session through.
 */

import type { TextBinding } from "./text-binding.js";

export type AuthoringSessionStatus =
  | "bootstrapping"
  | "live"
  | "clean"
  | "modified"
  | "commitPending"
  | "commitRejected"
  | "disconnectedReadable"
  | "offlineModified"
  | "replaying"
  | "migrationBlocked"
  | "dependencyBlocked"
  | "policyConflict"
  | "resyncRequired"
  | "recoveryRequired";

export type AuthoringExchangeMode =
  | "incremental"
  | "bootstrap"
  | "optimisticDocument";

export interface AuthoringResourceIdentity {
  entity: string;
  resourceKey: string;
}

export interface QueuedAuthoringOperation<TDocument = unknown> {
  operationId: string;
  resource: AuthoringResourceIdentity;
  schemaVersion: number;
  exchangeMode: AuthoringExchangeMode;
  baseRevision: string;
  updateBase64?: string;
  document?: TDocument;
  retryCount: number;
  state: "queued" | "sending" | "blocked";
  rejectionCategory?: string;
}

/** The replica operations a collaborative draft materialises. */
export interface AuthoringDraftOperations {
  /** Frontier of the replica state the operations extend. */
  baseFrontierBase64: string;
  /** Loro update from that frontier to the state the draft materialises. */
  updateBase64: string;
  /** The nonce of every peer block the operations are written under. */
  peerNonces: readonly string[];
}

export interface AuthoringSession<TDocument> {
  resource: AuthoringResourceIdentity;
  policy: "collaborative" | "optimisticDocument";
  status: AuthoringSessionStatus;
  schemaVersion: number;
  acceptedRevision: string;
  supportedExchangeModes: readonly AuthoringExchangeMode[];
  baseline: TDocument;
  draft: TDocument;
  validation: unknown;
  queuedOperations: readonly QueuedAuthoringOperation<TDocument>[];
  /**
   * Recorded while a collaborative session has pending work and a controller
   * that can record it, and dropped once the draft is set any other way.
   */
  draftOperations?: AuthoringDraftOperations;
  lastRejection?: {
    category: string;
    message: string;
  };
}

export interface AuthoringRuntimeState {
  sessions: Readonly<Record<string, AuthoringSession<unknown>>>;
}

export interface RedactedAuthoringSessionDiagnostic {
  resource: AuthoringResourceIdentity;
  policy: AuthoringSession<unknown>["policy"];
  status: AuthoringSessionStatus;
  schemaVersion: number;
  queuedOperationCount: number;
  sendingOperationCount: number;
  blockedOperationCount: number;
  retryCount: number;
  rejectionCategories: readonly string[];
}

export interface RedactedAuthoringRuntimeDiagnostic {
  sessionCount: number;
  queuedOperationCount: number;
  blockedOperationCount: number;
  sessions: readonly RedactedAuthoringSessionDiagnostic[];
  redaction: "metadataOnly";
}

export type AuthoringRuntimeListener = () => void;

/**
 * Ephemeral document machinery owned by one authoring session.
 *
 * The runtime snapshot remains serialisable. Generated-plan replicas, editor
 * bindings and their subscriptions live here and are rebuilt from that
 * snapshot when the application starts again.
 */
export interface AuthoringSessionController<
  TDocument,
  TTextFieldPath extends string = string
> {
  currentDraft?: () => TDocument;
  replaceDraft?: (draft: TDocument) => void;
  /**
   * Take a draft another runtime persisted through the operations it carries.
   * Only a controller whose draft holds its replica's operations provides it.
   */
  importDraft?: (draft: TDocument) => void;
  adoptDocument?: (document: TDocument) => void;
  bindText?: (
    fieldPath: TTextFieldPath,
    identities?: Readonly<Record<string, string>>
  ) => TextBinding;
  stageText?: (
    fieldPath: TTextFieldPath,
    identities?: Readonly<Record<string, string>>
  ) => AuthoringTextStageController;
  importUpdateBase64?: (updateBase64: string) => void;
  importVersionedUpdateBase64?: (
    schemaVersion: number,
    updateBase64: string
  ) => void;
  exportUpdateBase64?: () => string;
  exportIncrementalUpdateBase64?: (acceptedFrontierBase64: string) => string;
  acceptedFrontierBase64?: () => string;
  /** Whether the replica holds every operation up to a frontier. */
  coversFrontierBase64?: (frontierBase64: string) => boolean;
  /** The draft as it stood at a frontier the replica holds. */
  draftAt?: (frontierBase64: string) => TDocument;
  /** The nonce of every peer block the replica's operations may be written under. */
  peerNonces?: () => readonly string[];
  /** Records that the replica holds operations written under the blocks `nonces` name. */
  includePeerNonces?: (nonces: readonly string[]) => void;
  dispose?: () => void;
}

export type AuthoringTextStageConfirmation =
  | { kind: "committed" }
  | {
      kind: "blocked";
      message: string;
      reason: "schemaChanged" | "targetDeleted";
    };

/** Private text branch created by a domain collaboration controller. */
export interface AuthoringTextStageController {
  readonly binding: TextBinding;
  confirm: () => AuthoringTextStageConfirmation;
  dispose: () => void;
}
