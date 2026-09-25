/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * React-free authoring state machine shared by collaborative and optimistic
 * projection resources.
 */

import { areJsonValuesEqual } from "./json-value-equality";
import type { TextBinding } from "./text-binding";

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
  dispose?: () => void;
}

/**
 * A controller whose replica records a session's draft as operations and
 * takes them back: {@link AuthoringSession.draftOperations}.
 */
type OperationRecordingController = AuthoringSessionController<unknown, string>
  & Required<Pick<
    AuthoringSessionController<unknown, string>,
    | "acceptedFrontierBase64"
    | "coversFrontierBase64"
    | "exportIncrementalUpdateBase64"
    | "importUpdateBase64"
  >>;

function recordsOperations(
  controller: AuthoringSessionController<unknown, string>
): controller is OperationRecordingController {
  return controller.acceptedFrontierBase64 !== undefined
    && controller.coversFrontierBase64 !== undefined
    && controller.exportIncrementalUpdateBase64 !== undefined
    && controller.importUpdateBase64 !== undefined;
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

interface OwnedAuthoringTextStage {
  closed: boolean;
  controller: AuthoringTextStageController;
}

interface OwnedAuthoringController {
  resource: AuthoringResourceIdentity;
  controller: AuthoringSessionController<unknown, string>;
  handle?: AuthoringSessionHandle<unknown, string>;
  bindingSubscriptions: Map<TextBinding, () => void>;
  textStages: Set<OwnedAuthoringTextStage>;
  draftSyncQueued: boolean;
  disposed: boolean;
  /**
   * Frontier of the newest accepted state the replica holds, which a session's
   * recorded operations extend: where the replica stood when attached, then
   * each accepted frontier it takes through the session handle.
   */
  acceptedBaseFrontierBase64?: string;
}

/**
 * Stable capability for one private Save/Cancel text branch.
 *
 * Edits stay out of the resource draft until confirmation. A rejected
 * confirmation leaves the branch and its exact editor history available.
 */
export class AuthoringTextStageHandle {
  constructor(
    private readonly runtime: AuthoringRuntime,
    private readonly owner: OwnedAuthoringController,
    private readonly owned: OwnedAuthoringTextStage
  ) {}

  get binding(): TextBinding {
    return this.owned.controller.binding;
  }

  get closed(): boolean {
    return this.owned.closed;
  }

  confirm(): AuthoringTextStageConfirmation {
    this.requireOpen();
    if (this.owner.disposed) {
      return {
        kind: "blocked",
        message: "The authored resource is no longer open.",
        reason: "schemaChanged"
      };
    }
    const result = this.owned.controller.confirm();
    if (result.kind === "committed") {
      this.runtime.syncControllerDraft(this.owner);
      this.close();
    }
    return result;
  }

  cancel(): void {
    this.requireOpen();
    this.close();
  }

  private close(): void {
    this.owned.controller.dispose();
    this.owned.closed = true;
    this.owner.textStages.delete(this.owned);
  }

  private requireOpen(): void {
    if (this.owned.closed) {
      throw new Error("This staged text branch is already closed.");
    }
  }
}

/** Stable live capability for one resource already opened in AuthoringRuntime. */
export class AuthoringSessionHandle<
  TDocument,
  TTextFieldPath extends string = string
> {
  constructor(
    private readonly runtime: AuthoringRuntime,
    private readonly owned: OwnedAuthoringController
  ) {}

  get resource(): AuthoringResourceIdentity {
    return { ...this.owned.resource };
  }

  state(): AuthoringSession<TDocument> {
    const state = this.runtime.session<TDocument>(this.owned.resource);
    if (state === undefined) {
      throw new Error(
        `No authoring session exists for ${JSON.stringify(authoringSessionId(this.owned.resource))}.`
      );
    }
    return state;
  }

  currentDraft(): TDocument {
    const controller = this.controller();
    return controller.currentDraft === undefined
      ? structuredClone(this.state().draft)
      : structuredClone(controller.currentDraft());
  }

  replaceDraft(draft: TDocument, validation?: unknown): void {
    this.runtime.modify(this.owned.resource, draft, validation);
  }

  /**
   * Import accepted operations before reconciling the retained local draft.
   * `updateBase64` holds what the replica lacks of the state accepted at
   * `acceptedFrontierBase64`. The baseline defaults to the draft the replica
   * holds at that frontier once it has taken the update.
   */
  adoptAccepted(input: {
    updateBase64: string;
    acceptedFrontierBase64: string;
    schemaVersion?: number;
    baseline?: TDocument;
    draft?: TDocument;
    acceptedRevision: string;
    validation?: unknown;
  }): void {
    const controller = this.controller();
    if (input.schemaVersion === undefined) {
      if (controller.importUpdateBase64 === undefined) {
        throw new Error(`${this.owned.resource.entity} authoring cannot import Loro updates.`);
      }
      controller.importUpdateBase64(input.updateBase64);
    } else {
      if (controller.importVersionedUpdateBase64 === undefined) {
        throw new Error(
          `${this.owned.resource.entity} authoring cannot import versioned Loro updates.`
        );
      }
      controller.importVersionedUpdateBase64(
        input.schemaVersion,
        input.updateBase64
      );
    }
    takeAcceptedFrontier(this.owned, input.acceptedFrontierBase64);
    const baseline = () => input.baseline ?? this.draftAt(input.acceptedFrontierBase64);
    const draft = input.draft
      ?? controller.currentDraft?.()
      ?? baseline();
    if (this.state().queuedOperations.length > 0) {
      this.runtime.modify(this.owned.resource, draft);
      return;
    }
    this.runtime.adoptBaseline(this.owned.resource, {
      baseline: baseline(),
      draft,
      acceptedRevision: input.acceptedRevision,
      validation: input.validation
    });
  }

  bindText(
    fieldPath: TTextFieldPath,
    identities: Readonly<Record<string, string>> = {}
  ): TextBinding {
    const controller = this.controller();
    if (controller.bindText === undefined) {
      throw new Error(
        `${this.owned.resource.entity} authoring does not expose collaborative text fields.`
      );
    }
    const binding = controller.bindText(fieldPath, identities);
    if (!this.owned.bindingSubscriptions.has(binding)) {
      this.owned.bindingSubscriptions.set(
        binding,
        binding.subscribe(() => this.runtime.scheduleControllerDraftSync(this.owned))
      );
    }
    return binding;
  }

  stageText(
    fieldPath: TTextFieldPath,
    identities: Readonly<Record<string, string>> = {}
  ): AuthoringTextStageHandle {
    const controller = this.controller();
    if (controller.stageText === undefined) {
      throw new Error(
        `${this.owned.resource.entity} authoring does not expose staged text fields.`
      );
    }
    const owned: OwnedAuthoringTextStage = {
      closed: false,
      controller: controller.stageText(fieldPath, identities)
    };
    this.owned.textStages.add(owned);
    return new AuthoringTextStageHandle(this.runtime, this.owned, owned);
  }

  adoptDocument(document: TDocument): void {
    const controller = this.controller();
    if (controller.adoptDocument === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot adopt a document.`);
    }
    controller.adoptDocument(document);
  }

  /**
   * Import what the replica lacks of the state accepted at
   * `acceptedFrontierBase64`, and materialise the draft it leaves.
   */
  importUpdateBase64(updateBase64: string, acceptedFrontierBase64: string): void {
    const controller = this.controller();
    if (controller.importUpdateBase64 === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot import Loro updates.`);
    }
    controller.importUpdateBase64(updateBase64);
    takeAcceptedFrontier(this.owned, acceptedFrontierBase64);
    this.runtime.syncControllerDraft(this.owned);
  }

  /**
   * Import what the replica lacks of the state accepted at
   * `acceptedFrontierBase64`, and materialise the draft it leaves.
   */
  importVersionedUpdateBase64(
    schemaVersion: number,
    updateBase64: string,
    acceptedFrontierBase64: string
  ): void {
    const controller = this.controller();
    if (controller.importVersionedUpdateBase64 === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot import versioned Loro updates.`);
    }
    controller.importVersionedUpdateBase64(schemaVersion, updateBase64);
    takeAcceptedFrontier(this.owned, acceptedFrontierBase64);
    this.runtime.syncControllerDraft(this.owned);
  }

  /** The draft as it stood at a frontier the live replica holds, such as an accepted one. */
  draftAt(frontierBase64: string): TDocument {
    const operation = this.controller().draftAt;
    if (operation === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot read a draft at a frontier.`);
    }
    return operation(frontierBase64);
  }

  exportUpdateBase64(): string {
    const operation = this.controller().exportUpdateBase64;
    if (operation === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot export Loro updates.`);
    }
    return operation();
  }

  exportIncrementalUpdateBase64(acceptedFrontierBase64: string): string {
    const operation = this.controller().exportIncrementalUpdateBase64;
    if (operation === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot export incremental Loro updates.`);
    }
    return operation(acceptedFrontierBase64);
  }

  acceptedFrontierBase64(): string {
    const operation = this.controller().acceptedFrontierBase64;
    if (operation === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring has no Loro frontier.`);
    }
    return operation();
  }

  private controller(): AuthoringSessionController<TDocument, TTextFieldPath> {
    if (this.owned.disposed) {
      throw new Error(
        `The authoring session for ${JSON.stringify(authoringSessionId(this.owned.resource))} is closed.`
      );
    }
    return this.owned.controller as AuthoringSessionController<TDocument, TTextFieldPath>;
  }
}

export type AuthoringLeaveDecision =
  | { kind: "allow"; restoration: "notRequired" | "durable" }
  | { kind: "confirmDiscard" };

export function bootstrapAuthoringSession<TDocument>(input: {
  resource: AuthoringResourceIdentity;
  policy: AuthoringSession<TDocument>["policy"];
  schemaVersion: number;
  acceptedRevision: string;
  supportedExchangeModes: readonly AuthoringExchangeMode[];
  document: TDocument;
  validation?: unknown;
}): AuthoringSession<TDocument> {
  return {
    resource: { ...input.resource },
    policy: input.policy,
    status: "clean",
    schemaVersion: input.schemaVersion,
    acceptedRevision: input.acceptedRevision,
    supportedExchangeModes: [...input.supportedExchangeModes],
    baseline: structuredClone(input.document),
    draft: structuredClone(input.document),
    validation: structuredClone(input.validation),
    queuedOperations: []
  };
}

export function startAuthoringSession<TDocument>(input: {
  resource: AuthoringResourceIdentity;
  policy: AuthoringSession<TDocument>["policy"];
  schemaVersion: number;
  acceptedRevision: string;
  supportedExchangeModes: readonly AuthoringExchangeMode[];
  baseline: TDocument;
  draft: TDocument;
  validation?: unknown;
}): AuthoringSession<TDocument> {
  const clean = documentsEqual(input.baseline, input.draft);
  return {
    resource: { ...input.resource },
    policy: input.policy,
    status: clean ? "clean" : "modified",
    schemaVersion: input.schemaVersion,
    acceptedRevision: input.acceptedRevision,
    supportedExchangeModes: [...input.supportedExchangeModes],
    baseline: structuredClone(input.baseline),
    draft: structuredClone(input.draft),
    validation: structuredClone(input.validation),
    queuedOperations: []
  };
}

/** Statuses whose draft may change; a blocked session's draft stays as it is. */
const MODIFIABLE_STATUSES: readonly AuthoringSessionStatus[] = [
  "clean",
  "live",
  "modified",
  "commitPending",
  "replaying",
  "commitRejected",
  "disconnectedReadable",
  "offlineModified",
  "policyConflict"
];

export function modifyAuthoringSession<TDocument>(
  session: AuthoringSession<TDocument>,
  draft: TDocument,
  validation: unknown = session.validation
): AuthoringSession<TDocument> {
  requireStatus(session, "modify", ...MODIFIABLE_STATUSES);
  const draftMatchesBaseline = documentsEqual(session.baseline, draft);
  const hasQueuedOperations = session.queuedOperations.length > 0;
  return {
    ...session,
    status: session.status === "disconnectedReadable"
      || session.status === "offlineModified"
      ? hasQueuedOperations || !draftMatchesBaseline
        ? "offlineModified"
        : "disconnectedReadable"
      : hasQueuedOperations
        ? "commitPending"
        : draftMatchesBaseline
          ? "clean"
          : "modified",
    draft: structuredClone(draft),
    validation: structuredClone(validation),
    lastRejection: undefined
  };
}

export function queueAuthoringOperation<TDocument>(
  session: AuthoringSession<TDocument>,
  operation: Omit<QueuedAuthoringOperation<TDocument>, "resource" | "retryCount" | "state">
): AuthoringSession<TDocument> {
  requireStatus(
    session,
    "queue",
    "modified",
    "commitPending",
    "offlineModified",
    "commitRejected",
    "policyConflict"
  );
  if (session.queuedOperations.some(({ operationId }) => operationId === operation.operationId)) {
    return session;
  }
  const queued: QueuedAuthoringOperation<TDocument> = {
    ...operation,
    resource: { ...session.resource },
    retryCount: 0,
    state: "queued"
  };
  return {
    ...session,
    status: session.status === "offlineModified" ? "offlineModified" : "commitPending",
    queuedOperations: [...session.queuedOperations, queued]
  };
}

export function beginAuthoringReplay<TDocument>(
  session: AuthoringSession<TDocument>,
  operationId: string
): AuthoringSession<TDocument> {
  requireStatus(
    session,
    "replay",
    "commitPending",
    "offlineModified",
    "commitRejected",
    "replaying"
  );
  const operation = requireQueuedOperation(session, operationId);
  if (operation.state !== "queued") {
    throw new Error(
      `Cannot replay authoring operation ${JSON.stringify(operationId)} while it is ${operation.state}.`
    );
  }
  return {
    ...session,
    status: "replaying",
    queuedOperations: session.queuedOperations.map((candidate) =>
      candidate.operationId === operation.operationId
        ? { ...candidate, state: "sending" }
        : candidate
    )
  };
}

export function acknowledgeAuthoringOperation<TDocument>(input: {
  session: AuthoringSession<TDocument>;
  operationId: string;
  document: TDocument;
  acceptedRevision: string;
}): AuthoringSession<TDocument> {
  const operation = input.session.queuedOperations.find(
    ({ operationId }) => operationId === input.operationId
  );
  if (operation === undefined) {
    if (
      input.session.acceptedRevision === input.acceptedRevision
      && documentsEqual(input.session.baseline, input.document)
    ) {
      return input.session;
    }
    throw new Error(
      `Unknown queued authoring operation ${JSON.stringify(input.operationId)}.`
    );
  }
  const remaining = input.session.queuedOperations.filter(
    ({ operationId }) => operationId !== operation.operationId
  );
  const hasUncommittedChanges = !documentsEqual(
    input.document,
    input.session.draft
  );
  const retainedDraft = hasUncommittedChanges
    ? input.session.draft
    : input.document;
  const hasReplayableOperation = remaining.some(
    ({ state }) => state !== "blocked"
  );
  const hasBlockedOperation = remaining.some(
    ({ state }) => state === "blocked"
  );
  return {
    ...input.session,
    status: remaining.length > 0
      ? hasReplayableOperation
        ? "commitPending"
        : hasBlockedOperation
          ? "commitRejected"
          : "commitPending"
      : hasUncommittedChanges
        ? "modified"
        : "clean",
    acceptedRevision: input.acceptedRevision,
    baseline: structuredClone(input.document),
    draft: structuredClone(retainedDraft),
    queuedOperations: remaining,
    lastRejection: undefined
  };
}

export function adoptAuthoringBaseline<TDocument>(input: {
  session: AuthoringSession<TDocument>;
  baseline: TDocument;
  draft?: TDocument;
  acceptedRevision?: string;
  validation?: unknown;
}): AuthoringSession<TDocument> {
  const draft = input.draft === undefined
    ? resolveAuthoringDraftForBaselineAdoption(input.session, input.baseline)
    : input.draft;
  return {
    ...input.session,
    status: documentsEqual(input.baseline, draft) ? "clean" : "modified",
    acceptedRevision:
      input.acceptedRevision ?? input.session.acceptedRevision,
    baseline: structuredClone(input.baseline),
    draft: structuredClone(draft),
    validation: structuredClone(input.validation ?? input.session.validation),
    queuedOperations: [],
    lastRejection: undefined
  };
}

export function resolveAuthoringDraftForBaselineAdoption<TDocument>(
  session: AuthoringSession<TDocument>,
  nextBaseline: TDocument
): TDocument {
  return documentsEqual(session.baseline, session.draft)
    ? nextBaseline
    : session.draft;
}

export function discardAuthoringChanges<TDocument>(
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  return {
    ...session,
    status: "clean",
    draft: structuredClone(session.baseline),
    queuedOperations: [],
    lastRejection: undefined
  };
}

export function rejectAuthoringOperation<TDocument>(input: {
  session: AuthoringSession<TDocument>;
  operationId: string;
  category: string;
  message: string;
  retryable: boolean;
}): AuthoringSession<TDocument> {
  requireQueuedOperation(input.session, input.operationId);
  return {
    ...input.session,
    status: "commitRejected",
    queuedOperations: input.session.queuedOperations.map((candidate) =>
      candidate.operationId === input.operationId
        ? {
            ...candidate,
            retryCount: candidate.retryCount + 1,
            state: input.retryable ? "queued" : "blocked",
            rejectionCategory: input.category
          }
        : candidate
    ),
    lastRejection: {
      category: input.category,
      message: input.message
    }
  };
}

export function supersedeBlockedAuthoringOperations<TDocument>(
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  const remaining = session.queuedOperations.filter(
    ({ state }) => state !== "blocked"
  );
  if (remaining.length === session.queuedOperations.length) {
    return session;
  }
  return {
    ...session,
    status: remaining.length > 0
      ? "commitPending"
      : documentsEqual(session.baseline, session.draft)
        ? "clean"
        : "modified",
    queuedOperations: remaining,
    lastRejection: undefined
  };
}

export function disconnectAuthoringSession<TDocument>(
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  return {
    ...session,
    status:
      session.status === "modified"
      || session.status === "commitPending"
      || session.status === "commitRejected"
      || session.status === "replaying"
        ? "offlineModified"
        : "disconnectedReadable"
  };
}

export function reconnectAuthoringSession<TDocument>(
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  requireStatus(session, "reconnect", "disconnectedReadable", "offlineModified");
  const hasReplayableOperation = session.queuedOperations.some(
    ({ state }) => state === "queued"
  );
  const hasBlockedOperation = session.queuedOperations.some(
    ({ state }) => state === "blocked"
  );
  return {
    ...session,
    status: hasReplayableOperation
      ? "replaying"
      : hasBlockedOperation
        ? "commitRejected"
      : documentsEqual(session.baseline, session.draft)
        ? "clean"
        : "modified"
  };
}

export function blockAuthoringSession<TDocument>(
  session: AuthoringSession<TDocument>,
  status: Extract<
    AuthoringSessionStatus,
    | "migrationBlocked"
    | "dependencyBlocked"
    | "policyConflict"
    | "resyncRequired"
    | "recoveryRequired"
  >
): AuthoringSession<TDocument> {
  return { ...session, status };
}

/**
 * Whether a session holds something a reload cannot produce again.
 *
 * A clean session's draft, baseline, and validation all come back from the
 * server on the next load, so keeping it costs storage and carries nothing
 * forward. Local intent — a diverged draft, or an operation queued but not yet
 * accepted — is the only part that is lost if it is not written down.
 *
 * This is the line {@link authoringLeaveDecision} draws, and the "durable"
 * restoration it promises the user is the persisted snapshot.
 */
export function authoringSessionRequiresDurableRestoration<TDocument>(
  session: AuthoringSession<TDocument>
): boolean {
  if (session.queuedOperations.length > 0) {
    return true;
  }
  return session.status !== "clean"
    && session.status !== "live"
    && session.status !== "disconnectedReadable";
}

export function authoringLeaveDecision<TDocument>(
  session: AuthoringSession<TDocument>,
  durableRestorationAvailable: boolean
): AuthoringLeaveDecision {
  if (!authoringSessionRequiresDurableRestoration(session)) {
    return { kind: "allow", restoration: "notRequired" };
  }
  return durableRestorationAvailable
    ? { kind: "allow", restoration: "durable" }
    : { kind: "confirmDiscard" };
}

/**
 * Whether a session can take a newly accepted baseline right now.
 *
 * Adopting one discards the session's queued operations, which is right when
 * nothing is pending and wrong when a commit is in flight: the save that
 * queued it is still waiting to acknowledge it. A session with queued work
 * keeps that work and takes the baseline once it settles.
 */
export function authoringSessionAcceptsBaseline<TDocument>(
  session: AuthoringSession<TDocument>
): boolean {
  return session.queuedOperations.length === 0;
}

export type AuthoringReplayDecision =
  | { kind: "skip" }
  | { kind: "replay"; exchangeMode: AuthoringExchangeMode }
  | {
      kind: "blocked";
      reason: "migrationBlocked" | "dependencyBlocked" | "recoveryRequired";
    };

/**
 * Whether one queued operation can be replayed after a reconnect.
 *
 * An operation carries the schema version, exchange mode, and base frontier it
 * was built against. Replaying one whose ground has moved would commit
 * something the author never composed, so each mismatch resolves to the block
 * an operator can act on rather than to a silent drop or a blind retry.
 */
export function authoringReplayDecision<TDocument>(
  operation: QueuedAuthoringOperation<TDocument>,
  expectedSchemaVersion: number
): AuthoringReplayDecision {
  if (operation.state !== "queued") {
    return { kind: "skip" };
  }
  if (operation.document === undefined) {
    return { kind: "blocked", reason: "recoveryRequired" };
  }
  if (operation.schemaVersion !== expectedSchemaVersion) {
    return { kind: "blocked", reason: "migrationBlocked" };
  }
  if (operation.exchangeMode === "incremental" && !operation.baseRevision) {
    return { kind: "blocked", reason: "dependencyBlocked" };
  }
  return { kind: "replay", exchangeMode: operation.exchangeMode };
}

export function createAuthoringOperationId(): string {
  return globalThis.crypto.randomUUID();
}

export function authoringSessionId(resource: AuthoringResourceIdentity): string {
  return `${resource.entity}:${resource.resourceKey}`;
}

export function redactedAuthoringRuntimeDiagnostic(
  runtime: AuthoringRuntime | AuthoringRuntimeState
): RedactedAuthoringRuntimeDiagnostic {
  const state = runtime instanceof AuthoringRuntime
    ? runtime.getSnapshot()
    : runtime;
  const sessions = Object.values(state.sessions)
    .map((session): RedactedAuthoringSessionDiagnostic => {
      const rejectionCategories = new Set<string>();
      for (const operation of session.queuedOperations) {
        if (operation.rejectionCategory !== undefined) {
          rejectionCategories.add(operation.rejectionCategory);
        }
      }
      if (session.lastRejection !== undefined) {
        rejectionCategories.add(session.lastRejection.category);
      }
      return {
        resource: { ...session.resource },
        policy: session.policy,
        status: session.status,
        schemaVersion: session.schemaVersion,
        queuedOperationCount: session.queuedOperations.length,
        sendingOperationCount: session.queuedOperations.filter(
          ({ state }) => state === "sending"
        ).length,
        blockedOperationCount: session.queuedOperations.filter(
          ({ state: operationState }) => operationState === "blocked"
        ).length,
        retryCount: session.queuedOperations.reduce(
          (total, operation) => total + operation.retryCount,
          0
        ),
        rejectionCategories: [...rejectionCategories].sort()
      };
    })
    .sort((left, right) =>
      authoringSessionId(left.resource).localeCompare(
        authoringSessionId(right.resource)
      )
    );
  return {
    sessionCount: sessions.length,
    queuedOperationCount: sessions.reduce(
      (total, session) => total + session.queuedOperationCount,
      0
    ),
    blockedOperationCount: sessions.reduce(
      (total, session) => total + session.blockedOperationCount,
      0
    ),
    sessions,
    redaction: "metadataOnly"
  };
}

/**
 * The single React-free owner of authoring sessions and offline operations.
 *
 * UI bindings may subscribe to this store and the application may persist
 * its snapshot, but neither owns a second draft lifecycle.
 */
export class AuthoringRuntime {
  readonly #listeners = new Set<AuthoringRuntimeListener>();
  readonly #controllers = new Map<string, OwnedAuthoringController>();
  #state: AuthoringRuntimeState;
  #carried: Readonly<Record<string, AuthoringSession<unknown>>> = {};
  #batchDepth = 0;
  #batchDirty = false;

  /**
   * Start from a persisted snapshot, as after a restart. An operation that was
   * being sent when the snapshot was taken is queued again.
   */
  constructor(initial: AuthoringRuntimeState = { sessions: {} }) {
    this.#state = cloneRuntimeState(initial, true);
  }

  getSnapshot = (): AuthoringRuntimeState => this.#state;

  subscribe = (listener: AuthoringRuntimeListener): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /**
   * Apply several transitions and notify subscribers once, after the last one.
   *
   * Every subscriber sees every publish, so a loop that opens or adopts one
   * session per record would otherwise re-render each binding and rewrite the
   * persisted snapshot once per record. Batches nest; the outermost one
   * notifies.
   */
  batch(work: () => void): void {
    this.#batchDepth += 1;
    try {
      work();
    } finally {
      this.#batchDepth -= 1;
    }
    if (this.#batchDepth === 0 && this.#batchDirty) {
      this.#batchDirty = false;
      this.#notify();
    }
  }

  /**
   * Adopt the sessions another live runtime persisted on top of this one's.
   *
   * A snapshot carries only the sessions that
   * {@link authoringSessionRequiresDurableRestoration} keeps, so a session it
   * omits means "nothing pending for this resource", not "this resource is
   * gone". Restoring therefore overwrites what the snapshot names and leaves
   * everything else where it is.
   *
   * A collaborative draft materialises operations the other runtime holds and
   * sends itself, so this runtime never writes it into a replica. A
   * collaborative session with a live controller is restored only through
   * {@link AuthoringSessionController.importDraft}; without it the live session
   * stays, and the edits arrive as accepted state. A collaborative session this
   * runtime has no controller for is carried rather than adopted: it is
   * persisted with this runtime's own sessions ({@link persistedState}) until a
   * later snapshot omits it, and never attached here.
   */
  restore(snapshot: AuthoringRuntimeState): void {
    const restored = cloneRuntimeState(snapshot);
    const sessions = { ...this.#state.sessions };
    const carried: Record<string, AuthoringSession<unknown>> = {};
    let changed = false;
    for (const [id, session] of Object.entries(restored.sessions)) {
      const live = this.#state.sessions[id];
      const controller = this.#controllers.get(id)?.controller;
      if (session.policy === "collaborative" && controller === undefined) {
        carried[id] = session;
        continue;
      }
      if (documentsEqual(session, live)) {
        continue;
      }
      if (live?.policy === "collaborative" && controller !== undefined) {
        if (controller.importDraft === undefined) {
          continue;
        }
        controller.importDraft(session.draft);
      } else {
        controller?.replaceDraft?.(session.draft);
      }
      sessions[id] = session;
      changed = true;
    }
    this.#carried = carried;
    if (changed) {
      this.#publish({ sessions });
    }
  }

  /**
   * The sessions to persist: this runtime's own, and each session carried from
   * another runtime where this runtime has nothing pending of its own.
   */
  persistedState(): AuthoringRuntimeState {
    const sessions = { ...this.#state.sessions };
    for (const [id, session] of Object.entries(this.#carried)) {
      const own = sessions[id];
      if (own === undefined || !authoringSessionRequiresDurableRestoration(own)) {
        sessions[id] = session;
      }
    }
    return { sessions };
  }

  session<TDocument>(
    resource: AuthoringResourceIdentity
  ): AuthoringSession<TDocument> | undefined {
    return this.#state.sessions[authoringSessionId(resource)] as
      | AuthoringSession<TDocument>
      | undefined;
  }

  /**
   * Install the one live controller for an opened resource, or return its
   * existing stable session handle. The controller itself never enters the
   * serialisable runtime snapshot.
   */
  ensureController<TDocument, TTextFieldPath extends string>(
    resource: AuthoringResourceIdentity,
    create: () => AuthoringSessionController<TDocument, TTextFieldPath>
  ): AuthoringSessionHandle<TDocument, TTextFieldPath> {
    const id = authoringSessionId(resource);
    const session = this.session<TDocument>(resource);
    if (session === undefined) {
      throw new Error(`Cannot attach live authoring before opening ${JSON.stringify(id)}.`);
    }
    const existing = this.#controllers.get(id);
    if (existing !== undefined) {
      return existing.handle as AuthoringSessionHandle<TDocument, TTextFieldPath>;
    }
    const controller = create() as AuthoringSessionController<unknown, string>;
    const recording = recordsOperations(controller);
    const owned: OwnedAuthoringController = {
      resource: { ...resource },
      controller,
      bindingSubscriptions: new Map(),
      textStages: new Set(),
      draftSyncQueued: false,
      disposed: false,
      ...(recording ? { acceptedBaseFrontierBase64: controller.acceptedFrontierBase64() } : {})
    };
    // Recorded operations are taken as themselves, so edits the replica
    // already holds are not authored again. A replica without their base holds
    // other history, and a blocked session's draft stays as it is: both take
    // the draft as a document.
    const operations = session.draftOperations;
    if (
      recording
      && operations !== undefined
      && MODIFIABLE_STATUSES.includes(session.status)
      && controller.coversFrontierBase64(operations.baseFrontierBase64)
    ) {
      controller.importUpdateBase64(operations.updateBase64);
    } else {
      controller.replaceDraft?.(session.draft);
    }
    owned.handle = new AuthoringSessionHandle<unknown, string>(this, owned);
    this.#controllers.set(id, owned);
    this.syncControllerDraft(owned);
    const synced = this.session<unknown>(resource);
    if (synced !== undefined) {
      const recorded = recordDraftOperations(owned, synced);
      if (recorded !== synced) {
        this.#publish({ sessions: { ...this.#state.sessions, [id]: recorded } });
      }
    }
    return owned.handle as AuthoringSessionHandle<TDocument, TTextFieldPath>;
  }

  authoringSession<TDocument, TTextFieldPath extends string = string>(
    resource: AuthoringResourceIdentity
  ): AuthoringSessionHandle<TDocument, TTextFieldPath> | undefined {
    const owned = this.#controllers.get(authoringSessionId(resource));
    return owned === undefined
      ? undefined
      : owned.handle as AuthoringSessionHandle<TDocument, TTextFieldPath>;
  }

  /** Domain adapters use their runtime-owned controller for non-text commands. */
  controller<TController>(
    resource: AuthoringResourceIdentity
  ): TController | undefined {
    return this.#controllers.get(authoringSessionId(resource))?.controller as
      | TController
      | undefined;
  }

  replaceController<TDocument, TTextFieldPath extends string>(
    resource: AuthoringResourceIdentity,
    controller: AuthoringSessionController<TDocument, TTextFieldPath>
  ): AuthoringSessionHandle<TDocument, TTextFieldPath> {
    this.detachController(resource);
    return this.ensureController(resource, () => controller);
  }

  detachController(resource: AuthoringResourceIdentity): void {
    const id = authoringSessionId(resource);
    const owned = this.#controllers.get(id);
    if (owned === undefined) {
      return;
    }
    this.#controllers.delete(id);
    this.#disposeController(owned);
  }

  open<TDocument>(input: Parameters<typeof startAuthoringSession<TDocument>>[0]): void {
    this.#setSession(startAuthoringSession(input));
  }

  bootstrap<TDocument>(
    input: Parameters<typeof bootstrapAuthoringSession<TDocument>>[0]
  ): void {
    this.#setSession(bootstrapAuthoringSession(input));
  }

  modify<TDocument>(
    resource: AuthoringResourceIdentity,
    draft: TDocument,
    validation?: unknown
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    const owned = this.#controllers.get(authoringSessionId(resource));
    const controller = owned?.controller as
      | AuthoringSessionController<TDocument, string>
      | undefined;
    controller?.replaceDraft?.(draft);
    const materializedDraft = controller?.currentDraft?.() ?? draft;
    this.#setSession(
      modifyAuthoringSession(session, materializedDraft, validation),
      false
    );
  }

  adoptBaseline<TDocument>(
    resource: AuthoringResourceIdentity,
    input: Omit<Parameters<typeof adoptAuthoringBaseline<TDocument>>[0], "session">
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(adoptAuthoringBaseline({ session, ...input }));
  }

  queue<TDocument>(
    resource: AuthoringResourceIdentity,
    operation: Omit<QueuedAuthoringOperation<TDocument>, "resource" | "retryCount" | "state">
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(queueAuthoringOperation(session, operation));
  }

  beginReplay<TDocument>(
    resource: AuthoringResourceIdentity,
    operationId: string
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(beginAuthoringReplay(session, operationId));
  }

  acknowledge<TDocument>(input: {
    resource: AuthoringResourceIdentity;
    operationId: string;
    document: TDocument;
    acceptedRevision: string;
  }): void {
    const session = this.#requireSession<TDocument>(input.resource);
    this.#setSession(acknowledgeAuthoringOperation({ session, ...input }));
  }

  /**
   * Acknowledge an operation only while it is still owned by this runtime.
   *
   * Projection reconciliation can consume an accepted operation before the
   * originating async call settles. That completion race is not an unknown
   * operation error: the accepted state already owns the outcome.
   */
  acknowledgeIfQueued<TDocument>(input: {
    resource: AuthoringResourceIdentity;
    operationId: string;
    document: TDocument;
    acceptedRevision: string;
  }): boolean {
    const session = this.session<TDocument>(input.resource);
    if (
      session === undefined
      || !session.queuedOperations.some(
        ({ operationId }) => operationId === input.operationId
      )
    ) {
      return false;
    }
    this.#setSession(acknowledgeAuthoringOperation({ session, ...input }));
    return true;
  }

  reject<TDocument>(input: {
    resource: AuthoringResourceIdentity;
    operationId: string;
    category: string;
    message: string;
    retryable: boolean;
  }): void {
    const session = this.#requireSession<TDocument>(input.resource);
    this.#setSession(rejectAuthoringOperation({ session, ...input }));
  }

  /**
   * Reject an operation only while it is still owned by this runtime.
   *
   * Accepted projection reconciliation can consume an operation before the
   * originating async call settles. That completion race is not an unknown
   * operation error: the accepted state already owns the outcome.
   */
  rejectIfQueued<TDocument>(input: {
    resource: AuthoringResourceIdentity;
    operationId: string;
    category: string;
    message: string;
    retryable: boolean;
  }): boolean {
    const session = this.session<TDocument>(input.resource);
    if (
      session === undefined
      || !session.queuedOperations.some(
        ({ operationId }) => operationId === input.operationId
      )
    ) {
      return false;
    }
    this.#setSession(rejectAuthoringOperation({ session, ...input }));
    return true;
  }

  supersedeBlocked<TDocument>(
    resource: AuthoringResourceIdentity
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(supersedeBlockedAuthoringOperations(session));
  }

  disconnect(resource?: AuthoringResourceIdentity): void {
    this.#mapSelected(resource, (session) => disconnectAuthoringSession(session));
  }

  reconnect(resource?: AuthoringResourceIdentity): void {
    this.#mapSelected(resource, (session) => {
      if (
        session.status !== "disconnectedReadable"
        && session.status !== "offlineModified"
      ) {
        return session;
      }
      return reconnectAuthoringSession(session);
    });
  }

  block<TDocument>(
    resource: AuthoringResourceIdentity,
    status: Parameters<typeof blockAuthoringSession<TDocument>>[1]
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(blockAuthoringSession(session, status));
  }

  discard<TDocument>(resource: AuthoringResourceIdentity): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(discardAuthoringChanges(session));
  }

  remove(resource: AuthoringResourceIdentity): void {
    const id = authoringSessionId(resource);
    this.detachController(resource);
    if (!(id in this.#state.sessions)) {
      return;
    }
    const sessions = { ...this.#state.sessions };
    delete sessions[id];
    this.#publish({ sessions });
  }

  rename(from: AuthoringResourceIdentity, to: AuthoringResourceIdentity): void {
    const current = this.#requireSession(from);
    const fromId = authoringSessionId(from);
    const toId = authoringSessionId(to);
    const owned = this.#controllers.get(fromId);
    if (owned !== undefined) {
      const displaced = this.#controllers.get(toId);
      if (displaced !== undefined && displaced !== owned) {
        this.#disposeController(displaced);
      }
      this.#controllers.delete(fromId);
      owned.resource = { ...to };
      this.#controllers.set(toId, owned);
    }
    const sessions = { ...this.#state.sessions };
    delete sessions[fromId];
    const renamed = {
      ...current,
      resource: { ...to },
      queuedOperations: current.queuedOperations.map((operation) => ({
        ...operation,
        resource: { ...to }
      }))
    };
    sessions[toId] = renamed;
    this.#publish({ sessions });
  }

  /** Queue draft materialisation after a bound editor transaction finishes. */
  scheduleControllerDraftSync(owned: OwnedAuthoringController): void {
    if (owned.draftSyncQueued) {
      return;
    }
    owned.draftSyncQueued = true;
    queueMicrotask(() => {
      owned.draftSyncQueued = false;
      if (this.#controllers.get(authoringSessionId(owned.resource)) === owned) {
        this.syncControllerDraft(owned);
      }
    });
  }

  /** Materialise the controller into the serialisable draft without writing back. */
  syncControllerDraft(owned: OwnedAuthoringController): void {
    const controller = owned.controller;
    if (controller.currentDraft === undefined) {
      return;
    }
    const session = this.session<unknown>(owned.resource);
    if (session === undefined) {
      return;
    }
    const draft = controller.currentDraft();
    if (documentsEqual(session.draft, draft)) {
      return;
    }
    this.#setSession(modifyAuthoringSession(session, draft), false);
  }

  dispose(): void {
    for (const owned of this.#controllers.values()) {
      this.#disposeController(owned);
    }
    this.#controllers.clear();
    this.#listeners.clear();
  }

  #requireSession<TDocument>(
    resource: AuthoringResourceIdentity
  ): AuthoringSession<TDocument> {
    const session = this.session<TDocument>(resource);
    if (session === undefined) {
      throw new Error(
        `No authoring session exists for ${JSON.stringify(authoringSessionId(resource))}.`
      );
    }
    return session;
  }

  // Transitions build fresh session objects and clone the documents they take
  // in, so the stored session needs no further copy. Keeping the transition's
  // object means a field a transition left alone (a baseline while the draft
  // changes, every other session while one changes) keeps its identity, and
  // bindings that memoise on those identities stay put.
  #setSession<TDocument>(
    session: AuthoringSession<TDocument>,
    alignController: boolean = true
  ): void {
    const id = authoringSessionId(session.resource);
    const owned = this.#controllers.get(id);
    if (alignController) {
      owned?.controller.replaceDraft?.(session.draft);
    }
    this.#publish({
      sessions: {
        ...this.#state.sessions,
        [id]: owned === undefined
          ? withoutStaleDraftOperations(this.#state.sessions[id], session)
          : recordDraftOperations(owned, session)
      }
    });
  }

  #mapSelected(
    resource: AuthoringResourceIdentity | undefined,
    transition: (session: AuthoringSession<unknown>) => AuthoringSession<unknown>
  ): void {
    const ids = resource === undefined
      ? Object.keys(this.#state.sessions)
      : [authoringSessionId(resource)];
    let changed = false;
    const sessions = { ...this.#state.sessions };
    for (const id of ids) {
      const session = sessions[id];
      if (session === undefined) {
        continue;
      }
      const next = transition(session);
      if (next !== session) {
        sessions[id] = next;
        changed = true;
      }
    }
    if (changed) {
      this.#publish({ sessions });
    }
  }

  #publish(state: AuthoringRuntimeState): void {
    this.#state = state;
    if (this.#batchDepth > 0) {
      this.#batchDirty = true;
      return;
    }
    this.#notify();
  }

  #notify(): void {
    for (const listener of this.#listeners) {
      listener();
    }
  }

  #disposeController(owned: OwnedAuthoringController): void {
    if (owned.disposed) {
      return;
    }
    owned.disposed = true;
    for (const unsubscribe of owned.bindingSubscriptions.values()) {
      unsubscribe();
    }
    owned.bindingSubscriptions.clear();
    for (const stage of owned.textStages) {
      if (!stage.closed) {
        stage.controller.dispose();
        stage.closed = true;
      }
    }
    owned.textStages.clear();
    owned.controller.dispose?.();
  }
}

/**
 * Records what a session's replica holds beyond the newest accepted state it
 * has taken, while the session has anything pending.
 */
function recordDraftOperations<TDocument>(
  owned: OwnedAuthoringController,
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  const controller = owned.controller;
  const base = owned.acceptedBaseFrontierBase64;
  if (session.policy !== "collaborative" || base === undefined || !recordsOperations(controller)) {
    return session;
  }
  if (!authoringSessionRequiresDurableRestoration(session)) {
    return withoutDraftOperations(session);
  }
  const updateBase64 = controller.exportIncrementalUpdateBase64(base);
  const recorded = session.draftOperations;
  if (recorded?.baseFrontierBase64 === base && recorded.updateBase64 === updateBase64) {
    return session;
  }
  return { ...session, draftOperations: { baseFrontierBase64: base, updateBase64 } };
}

/** Moves a replica's accepted base to a frontier the server accepted. */
function takeAcceptedFrontier(owned: OwnedAuthoringController, acceptedFrontierBase64: string): void {
  if (owned.acceptedBaseFrontierBase64 !== undefined && recordsOperations(owned.controller)) {
    owned.acceptedBaseFrontierBase64 = acceptedFrontierBase64;
  }
}

/**
 * A session without a replica keeps its recorded operations while it is
 * pending and its draft is still the one they materialise.
 */
function withoutStaleDraftOperations<TDocument>(
  previous: AuthoringSession<unknown> | undefined,
  next: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  return previous !== undefined
    && authoringSessionRequiresDurableRestoration(next)
    && documentsEqual(previous.draft, next.draft)
    ? next
    : withoutDraftOperations(next);
}

function withoutDraftOperations<TDocument>(
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  if (session.draftOperations === undefined) {
    return session;
  }
  const next = { ...session };
  delete next.draftOperations;
  return next;
}

function cloneRuntimeState(
  state: AuthoringRuntimeState,
  recoverInterruptedReplay: boolean = false
): AuthoringRuntimeState {
  return {
    sessions: Object.fromEntries(
      Object.entries(state.sessions).map(([id, session]) => [
        id,
        {
          ...structuredClone(session),
          status:
            recoverInterruptedReplay && session.status === "replaying"
              ? "offlineModified"
              : session.status,
          queuedOperations: session.queuedOperations.map((operation) => ({
            ...structuredClone(operation),
            state:
              recoverInterruptedReplay && operation.state === "sending"
                ? "queued"
                : operation.state
          }))
        }
      ])
    )
  };
}

function requireQueuedOperation<TDocument>(
  session: AuthoringSession<TDocument>,
  operationId: string
): QueuedAuthoringOperation {
  const operation = session.queuedOperations.find(
    (candidate) => candidate.operationId === operationId
  );
  if (operation === undefined) {
    throw new Error(`Unknown queued authoring operation ${JSON.stringify(operationId)}.`);
  }
  return operation;
}

function requireStatus<TDocument>(
  session: AuthoringSession<TDocument>,
  transition: string,
  ...allowed: readonly AuthoringSessionStatus[]
): void {
  if (!allowed.includes(session.status)) {
    throw new Error(
      `Cannot ${transition} an authoring session while it is ${session.status}.`
    );
  }
}

function documentsEqual(left: unknown, right: unknown): boolean {
  return areJsonValuesEqual(left, right);
}
