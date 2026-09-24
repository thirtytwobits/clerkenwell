/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The persisted authoring runtime: its stored shape, and the conversions
 * between it and the live runtime state.
 */
import {
  authoringSessionRequiresDurableRestoration,
  type AuthoringExchangeMode,
  type AuthoringRuntimeState,
  type AuthoringSession,
  type AuthoringSessionStatus
} from "./authoring-session";

export interface PersistedAuthoringResourceIdentity {
  entity: string;
  resource_key: string;
}

export interface PersistedQueuedAuthoringOperation {
  operation_id: string;
  resource: PersistedAuthoringResourceIdentity;
  schema_version: number;
  exchange_mode: AuthoringExchangeMode;
  base_revision: string;
  update_base64?: string;
  document?: unknown;
  retry_count: number;
  state: "queued" | "sending" | "blocked";
  rejection_category?: string;
}

export interface PersistedAuthoringDraftOperations {
  base_frontier_base64: string;
  update_base64: string;
}

export interface PersistedAuthoringSessionState {
  resource: PersistedAuthoringResourceIdentity;
  policy: "collaborative" | "optimisticDocument";
  status: AuthoringSessionStatus;
  schema_version: number;
  accepted_revision: string;
  supported_exchange_modes: AuthoringExchangeMode[];
  baseline: unknown;
  draft: unknown;
  validation: unknown;
  queued_operations: PersistedQueuedAuthoringOperation[];
  draft_operations?: PersistedAuthoringDraftOperations;
  last_rejection?: unknown;
}

export interface PersistedAuthoringRuntimeState {
  sessions: Record<string, PersistedAuthoringSessionState>;
}

/**
 * Narrow the live runtime to what has to survive a restart.
 *
 * The snapshot is pending work, not a mirror of the runtime: a session the
 * server can hand back in full is dropped rather than written, so persistent
 * storage holds only local intent.
 */
export function toPersistedRuntime(
  value: AuthoringRuntimeState
): PersistedAuthoringRuntimeState {
  return {
    sessions: Object.fromEntries(
      Object.entries(value.sessions)
        .filter(([, session]) => authoringSessionRequiresDurableRestoration(session))
        .map(([id, session]) => [id, toPersistedSession(session)])
    )
  };
}

/** The sessions {@link toPersistedRuntime} would write, keyed by session id. */
export function durableSessions(
  value: AuthoringRuntimeState
): ReadonlyMap<string, AuthoringSession<unknown>> {
  return new Map(
    Object.entries(value.sessions)
      .filter(([, session]) => authoringSessionRequiresDurableRestoration(session))
  );
}

/**
 * Whether two durable-session maps hold the same session objects. The runtime
 * keeps a session's object until a transition replaces it, so identity is the
 * test of whether anything worth persisting changed.
 */
export function sameSessions(
  left: ReadonlyMap<string, AuthoringSession<unknown>>,
  right: ReadonlyMap<string, AuthoringSession<unknown>>
): boolean {
  if (left.size !== right.size) {
    return false;
  }
  for (const [id, session] of left) {
    if (right.get(id) !== session) {
      return false;
    }
  }
  return true;
}

export function fromPersistedRuntime(
  value: PersistedAuthoringRuntimeState
): AuthoringRuntimeState {
  return {
    sessions: Object.fromEntries(
      Object.entries(value.sessions).map(([id, session]) => [
        id,
        fromPersistedSession(session)
      ])
    )
  };
}

function toPersistedSession(
  session: AuthoringSession<unknown>
): PersistedAuthoringSessionState {
  return {
    resource: {
      entity: session.resource.entity,
      resource_key: session.resource.resourceKey
    },
    policy: session.policy,
    status: session.status,
    schema_version: session.schemaVersion,
    accepted_revision: session.acceptedRevision,
    supported_exchange_modes: [...session.supportedExchangeModes],
    // The documents are referenced rather than copied: the caller encodes the
    // snapshot as it stores it.
    baseline: session.baseline,
    draft: session.draft,
    validation: session.validation,
    queued_operations: session.queuedOperations.map((operation) => ({
      operation_id: operation.operationId,
      resource: {
        entity: operation.resource.entity,
        resource_key: operation.resource.resourceKey
      },
      schema_version: operation.schemaVersion,
      exchange_mode: operation.exchangeMode,
      base_revision: operation.baseRevision,
      ...(operation.updateBase64 === undefined
        ? {}
        : { update_base64: operation.updateBase64 }),
      ...(operation.document === undefined
        ? {}
        : { document: operation.document }),
      retry_count: operation.retryCount,
      state: operation.state,
      ...(operation.rejectionCategory === undefined
        ? {}
        : { rejection_category: operation.rejectionCategory })
    })),
    ...(session.draftOperations === undefined
      ? {}
      : {
          draft_operations: {
            base_frontier_base64: session.draftOperations.baseFrontierBase64,
            update_base64: session.draftOperations.updateBase64
          }
        }),
    ...(session.lastRejection === undefined
      ? {}
      : { last_rejection: structuredClone(session.lastRejection) })
  };
}

function fromPersistedSession(
  session: PersistedAuthoringSessionState
): AuthoringSession<unknown> {
  return {
    resource: {
      entity: session.resource.entity,
      resourceKey: session.resource.resource_key
    },
    policy: session.policy,
    status: session.status,
    schemaVersion: session.schema_version,
    acceptedRevision: session.accepted_revision,
    supportedExchangeModes: [...session.supported_exchange_modes],
    baseline: structuredClone(session.baseline),
    draft: structuredClone(session.draft),
    validation: structuredClone(session.validation),
    queuedOperations: session.queued_operations.map((operation) => ({
      operationId: operation.operation_id,
      resource: {
        entity: operation.resource.entity,
        resourceKey: operation.resource.resource_key
      },
      schemaVersion: operation.schema_version,
      exchangeMode: operation.exchange_mode,
      baseRevision: operation.base_revision,
      ...(operation.update_base64 === undefined
        ? {}
        : { updateBase64: operation.update_base64 }),
      ...(operation.document === undefined
        ? {}
        : { document: structuredClone(operation.document) }),
      retryCount: operation.retry_count,
      state: operation.state,
      ...(operation.rejection_category === undefined
        ? {}
        : { rejectionCategory: operation.rejection_category })
    })),
    ...(session.draft_operations === undefined
      ? {}
      : {
          draftOperations: {
            baseFrontierBase64: session.draft_operations.base_frontier_base64,
            updateBase64: session.draft_operations.update_base64
          }
        }),
    ...(isAuthoringRejection(session.last_rejection)
      ? { lastRejection: session.last_rejection }
      : {})
  };
}

function isAuthoringRejection(
  value: unknown
): value is { category: string; message: string } {
  return (
    value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && typeof (value as Record<string, unknown>).category === "string"
    && typeof (value as Record<string, unknown>).message === "string"
  );
}
