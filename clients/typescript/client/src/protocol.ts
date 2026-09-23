/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Projection session protocol: subscribe, resync, unsubscribe and mutate
 * commands, their accepted results, pushed events and typed rejections.
 */
import type {
  MutationName,
  MutationParams,
  ProjectionModel,
  ProjectionTransportMutationResult,
  ProjectionTransportPatch,
  ProjectionTransportSnapshot
} from "./projection-model";

export const PROJECTION_SUBSCRIBE_METHOD = "projection.subscribe";
export const PROJECTION_RESYNC_METHOD = "projection.resync";
export const PROJECTION_UNSUBSCRIBE_METHOD = "projection.unsubscribe";
export const PROJECTION_MUTATE_METHOD = "projection.mutate";
export const PROJECTION_UPDATE_NOTIFICATION = "projection.update";

export interface ProjectionSubscribeCommand {
  projection: string;
  params?: unknown;
  cursor?: ProjectionSubscribeCursor;
}

export interface ProjectionSubscribeCursor {
  revision: number;
}

export interface ProjectionResyncCommand {
  subscription_id: number;
}

export interface ProjectionUnsubscribeCommand {
  subscription_id: number;
}

export interface ProjectionMutationCommand<
  M extends ProjectionModel = ProjectionModel,
  TMutation extends MutationName<M> = MutationName<M>
> {
  mutation: TMutation;
  operation_id?: string;
  base_revision?: number;
  params: MutationParams<M, TMutation>;
}

export interface ProjectionSubscribeAccepted {
  subscription_id: number;
  revision: number;
  resume?: ProjectionSubscribeResume;
}

export type ProjectionSubscribeResume =
  | {
    kind: "up_to_date";
    revision: number;
  }
  | {
    kind: "patches";
    from_revision: number;
    revision: number;
    patch_count: number;
  }
  | {
    kind: "snapshot";
    from_revision: number;
    revision: number;
  };

export interface ProjectionResyncAccepted {
  subscription_id: number;
  from_revision: number;
  revision: number;
}

export interface ProjectionUnsubscribeAccepted {
  removed: boolean;
}

export interface ProjectionMutationAccepted<M extends ProjectionModel = ProjectionModel> {
  operation_id?: string;
  base_revision?: number;
  revision: number;
  result: ProjectionTransportMutationResult<M>;
}

export type ProjectionTransportEvent<M extends ProjectionModel = ProjectionModel> =
  | {
    kind: "snapshot";
    subscription_id: number;
    revision: number;
    snapshot: ProjectionTransportSnapshot<M>;
  }
  | {
    kind: "patch";
    subscription_id: number;
    from_revision: number;
    to_revision: number;
    patch: ProjectionTransportPatch<M>;
  };

export type ProjectionErrorCode =
  | "unknown_mutation"
  | "unsupported_mutation"
  | "unknown_projection"
  | "unsupported_projection"
  | "invalid_params"
  | "not_found"
  | "conflict"
  | "validation_failed"
  | "base_revision_in_future"
  | "cursor_ahead"
  | "stale_write"
  | "rate_limit"
  | "internal_error";

export interface ProjectionErrorEnvelope {
  code: ProjectionErrorCode;
  message: string;
  operation: "subscribe" | "resync" | "unsubscribe" | "mutate";
  retryable: boolean;
  name?: string;
  details?: unknown;
}

export function isProjectionTransportEvent<M extends ProjectionModel = ProjectionModel>(
  value: unknown
): value is ProjectionTransportEvent<M> {
  return isRecord(value) && typeof value.kind === "string";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
