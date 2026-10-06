/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The authoring state machine: pure transitions of one session. Applications
 * drive sessions through `AuthoringRuntime`, which owns every transition.
 */

import type { AuthoringExchangeMode, AuthoringResourceIdentity, AuthoringSession, AuthoringSessionStatus, QueuedAuthoringOperation } from "./authoring-session.js";
import { areJsonValuesEqual } from "./json-value-equality.js";

/**
 * The code a store refuses an import with when the importer must replace its
 * replica with one of the store's history.
 */
const RESYNC_REQUIRED = "collaboration_resync_required";

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

/**
 * Take an accepted baseline. A session that does not accept a draft holds its
 * baseline, draft and queued operations until its block is resolved.
 */
export function adoptAuthoringBaseline<TDocument>(input: {
  session: AuthoringSession<TDocument>;
  baseline: TDocument;
  draft?: TDocument;
  acceptedRevision?: string;
  validation?: unknown;
}): AuthoringSession<TDocument> {
  if (!authoringSessionAcceptsDraft(input.session)) {
    return input.session;
  }
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

/**
 * A session whose replica is replaced by one of another history: the
 * accepted state `accepted` at `acceptedRevision`, as a re-seeded document
 * leaves it. Operations of the old history are never sent, so the queue and
 * the recorded operations go. With nothing pending the session takes
 * `accepted`. Pending edits are kept, to be written again on the new replica,
 * when `accepted` is the document they were made from; otherwise the session
 * is held for recovery with its draft, over `accepted`.
 */
export function replaceAuthoringHistory<TDocument>(
  session: AuthoringSession<TDocument>,
  accepted: TDocument,
  acceptedRevision: string
): AuthoringSession<TDocument> {
  const replaced: AuthoringSession<TDocument> = {
    ...session,
    acceptedRevision,
    baseline: structuredClone(accepted),
    queuedOperations: [],
    lastRejection: undefined
  };
  delete replaced.draftOperations;
  if (!authoringSessionRequiresDurableRestoration(session)) {
    return { ...replaced, status: "clean", draft: structuredClone(accepted) };
  }
  const editable = authoringSessionAcceptsDraft(session) || session.status === "resyncRequired";
  if (editable && documentsEqual(session.baseline, accepted)) {
    return {
      ...replaced,
      status: documentsEqual(accepted, session.draft) ? "clean" : "modified"
    };
  }
  return { ...replaced, status: "recoveryRequired" };
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
  // The store holds a history the session's replica is not of: no operation
  // of that replica can be accepted until it is replaced, whatever a later
  // rejection says.
  const resync = input.category === RESYNC_REQUIRED || input.session.status === "resyncRequired";
  return {
    ...input.session,
    status: resync ? "resyncRequired" : "commitRejected",
    queuedOperations: input.session.queuedOperations.map((candidate) =>
      candidate.operationId === input.operationId
        ? {
            ...candidate,
            retryCount: candidate.retryCount + 1,
            state: input.retryable && !resync ? "queued" : "blocked",
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

/** The statuses {@link blockAuthoringSession} sets. */
const BLOCKED_STATUSES = Object.keys({
  migrationBlocked: true,
  dependencyBlocked: true,
  policyConflict: true,
  resyncRequired: true,
  recoveryRequired: true
} satisfies Record<Parameters<typeof blockAuthoringSession>[1], true>) as AuthoringSessionStatus[];

/** A blocked session stays blocked: the author or the server resolves a block, never connectivity. */
export function disconnectAuthoringSession<TDocument>(
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  if (BLOCKED_STATUSES.includes(session.status)) {
    return session;
  }
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

/**
 * Whether a session's draft may change right now. A blocked session's draft
 * stays as it is until the block is resolved.
 */
export function authoringSessionAcceptsDraft<TDocument>(
  session: AuthoringSession<TDocument>
): boolean {
  return MODIFIABLE_STATUSES.includes(session.status);
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

export function requireQueuedOperation<TDocument>(
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

export function requireStatus<TDocument>(
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

export function documentsEqual(left: unknown, right: unknown): boolean {
  return areJsonValuesEqual(left, right);
}
