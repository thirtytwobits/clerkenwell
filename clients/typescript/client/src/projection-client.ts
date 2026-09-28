/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The projection session protocol over an {@link RpcSocket}: the transport
 * `subscribeProjection` and `watchProjection` follow, and mutations.
 */
import type {
  MutationName,
  MutationParams,
  MutationResult,
  ProjectionModel,
  ProjectionName,
  ProjectionParams
} from "./projection-model";
import type { ProjectionTransport } from "./projection-subscription";
import {
  PROJECTION_MUTATE_METHOD,
  PROJECTION_RESYNC_METHOD,
  PROJECTION_SUBSCRIBE_METHOD,
  PROJECTION_UNSUBSCRIBE_METHOD,
  type ProjectionMutationAccepted,
  type ProjectionResyncAccepted,
  type ProjectionSubscribeAccepted,
  type ProjectionUnsubscribeAccepted
} from "./protocol";
import { RpcError, type RpcNotification, type RpcSocket, type RpcSocketState } from "./rpc-socket";

export class ProjectionClient<M extends ProjectionModel = ProjectionModel> implements ProjectionTransport<M> {
  constructor(readonly socket: RpcSocket) {}

  addNotificationListener(listener: (notification: RpcNotification) => void): () => void {
    return this.socket.addNotificationListener(listener);
  }

  addConnectionStateListener(listener: (state: RpcSocketState) => void): () => void {
    return this.socket.addConnectionStateListener(listener);
  }

  /** Whether the server refused a request, rather than the request being lost with its connection. */
  isRefusal(error: unknown): boolean {
    return error instanceof RpcError;
  }

  projectionSubscribe<K extends ProjectionName<M>>(
    projection: K,
    params: ProjectionParams<M, K>,
    options: { readonly cursorRevision?: number } = {}
  ): Promise<ProjectionSubscribeAccepted> {
    return this.socket.call(PROJECTION_SUBSCRIBE_METHOD, {
      projection,
      params,
      ...(options.cursorRevision === undefined ? {} : { cursor: { revision: options.cursorRevision } })
    });
  }

  projectionResync(subscriptionId: number): Promise<ProjectionResyncAccepted> {
    return this.socket.call(PROJECTION_RESYNC_METHOD, { subscription_id: subscriptionId });
  }

  projectionUnsubscribe(subscriptionId: number): Promise<ProjectionUnsubscribeAccepted> {
    return this.socket.call(PROJECTION_UNSUBSCRIBE_METHOD, { subscription_id: subscriptionId });
  }

  /** Runs a mutation, resolving with its result. */
  async projectionMutate<K extends MutationName<M>>(
    mutation: K,
    params: MutationParams<M, K>,
    options: { readonly operationId?: string; readonly baseRevision?: number } = {}
  ): Promise<MutationResult<M, K>> {
    const accepted = await this.socket.call<ProjectionMutationAccepted<M>>(PROJECTION_MUTATE_METHOD, {
      mutation,
      params,
      ...(options.operationId === undefined ? {} : { operation_id: options.operationId }),
      ...(options.baseRevision === undefined ? {} : { base_revision: options.baseRevision })
    });
    if (accepted.result.mutation !== mutation) {
      throw new Error(`Expected a ${mutation} result, received ${accepted.result.mutation}.`);
    }
    return accepted.result.value as MutationResult<M, K>;
  }
}
