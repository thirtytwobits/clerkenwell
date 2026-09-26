/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Projection subscriptions over any transport that speaks the projection
 * session protocol.
 */
import { applyProjectionEvent, createProjectionMaterializerState } from "./materialize";
import type { ProjectionCompositionPlans } from "./plans";
import type {
  ProjectionModel,
  ProjectionName,
  ProjectionParams,
  ProjectionPatch,
  ProjectionSnapshot
} from "./projection-model";
import {
  isProjectionTransportEvent,
  PROJECTION_UPDATE_NOTIFICATION,
  type ProjectionResyncAccepted,
  type ProjectionSubscribeAccepted,
  type ProjectionTransportEvent,
  type ProjectionUnsubscribeAccepted
} from "./protocol";

/** A message the server pushes; projection events arrive as `projection.update`. */
export interface ProjectionNotification {
  readonly method: string;
  readonly params?: unknown;
}

/**
 * A transport's connection state. Subscriptions open when it is `connected`;
 * every state other than `connecting` and `connected` loses them.
 */
export interface ProjectionConnectionState {
  readonly status: string;
}

/** What a one-shot subscription needs from a transport. */
export interface ProjectionSubscribeTransport<M extends ProjectionModel = ProjectionModel> {
  addNotificationListener(listener: (notification: ProjectionNotification) => void): () => void;
  projectionSubscribe<K extends ProjectionName<M>>(
    projection: K,
    params: ProjectionParams<M, K>
  ): Promise<ProjectionSubscribeAccepted>;
  projectionUnsubscribe(subscriptionId: number): Promise<ProjectionUnsubscribeAccepted>;
}

/** What a watch needs from a transport to follow it through reconnects. */
export interface ProjectionTransport<M extends ProjectionModel = ProjectionModel>
  extends ProjectionSubscribeTransport<M> {
  /** Registers a listener and replays the current state to it. */
  addConnectionStateListener(listener: (state: ProjectionConnectionState) => void): () => void;
  projectionResync(subscriptionId: number): Promise<ProjectionResyncAccepted>;
  /**
   * Whether a failed request was refused by the server rather than lost with
   * its connection. A refused subscribe fails a watch; a lost one waits for
   * the next connection.
   */
  isRefusal(error: unknown): boolean;
}

export interface ProjectionSubscription<
  M extends ProjectionModel,
  TProjection extends ProjectionName<M>
> {
  subscriptionId: number;
  snapshot: ProjectionSnapshot<M, TProjection>;
  unsubscribe: () => Promise<void>;
}

/** Subscribes once, resolving with the first snapshot and reporting later patches raw. */
export async function subscribeProjection<
  M extends ProjectionModel,
  TProjection extends ProjectionName<M>
>(options: {
  client: ProjectionSubscribeTransport<M>;
  projection: TProjection;
  params: ProjectionParams<M, TProjection>;
  onSnapshot?: (snapshot: ProjectionSnapshot<M, TProjection>) => void;
  onPatch?: (patch: ProjectionPatch<M, TProjection>) => void;
}): Promise<ProjectionSubscription<M, TProjection>> {
  let subscriptionId: number | null = null;
  const earlySnapshots = new Map<number, ProjectionSnapshot<M, TProjection>>();
  let resolveSnapshot!: (snapshot: ProjectionSnapshot<M, TProjection>) => void;
  let rejectSnapshot!: (error: unknown) => void;
  const snapshotPromise = new Promise<ProjectionSnapshot<M, TProjection>>((resolve, reject) => {
    resolveSnapshot = resolve;
    rejectSnapshot = reject;
  });
  const timeoutId = setTimeout(() => {
    rejectSnapshot(new Error(`Timed out waiting for ${options.projection} projection snapshot.`));
  }, 30_000);
  // The first snapshot settles the subscription, so a handler that throws on
  // it fails the subscription with that error. Later snapshots reach the
  // handler as patches do.
  let settled = false;
  const receiveSnapshot = (snapshot: ProjectionSnapshot<M, TProjection>): void => {
    if (settled) {
      options.onSnapshot?.(snapshot);
      return;
    }
    settled = true;
    try {
      options.onSnapshot?.(snapshot);
    } catch (error) {
      rejectSnapshot(error);
      return;
    }
    resolveSnapshot(snapshot);
  };
  const removeListener = options.client.addNotificationListener((notification) => {
    if (
      notification.method !== PROJECTION_UPDATE_NOTIFICATION
      || !isProjectionTransportEvent<M>(notification.params)
    ) {
      return;
    }
    const event = notification.params;
    if (
      event.kind === "snapshot"
      && event.snapshot.projection === options.projection
    ) {
      const snapshot = event.snapshot.value as ProjectionSnapshot<M, TProjection>;
      if (subscriptionId == null) {
        earlySnapshots.set(event.subscription_id, snapshot);
        return;
      }
      if (event.subscription_id !== subscriptionId) {
        return;
      }
      receiveSnapshot(snapshot);
      return;
    }
    if (
      subscriptionId != null
      && event.subscription_id === subscriptionId
      && event.kind === "patch"
      && event.patch.projection === options.projection
    ) {
      options.onPatch?.(event.patch.value as ProjectionPatch<M, TProjection>);
    }
  });

  try {
    const accepted = await options.client.projectionSubscribe(options.projection, options.params);
    subscriptionId = accepted.subscription_id;
    const earlySnapshot = earlySnapshots.get(subscriptionId);
    if (earlySnapshot !== undefined) {
      receiveSnapshot(earlySnapshot);
    }
    const snapshot = await snapshotPromise;
    const activeSubscriptionId = subscriptionId;
    return {
      subscriptionId: activeSubscriptionId,
      snapshot,
      unsubscribe: async () => {
        removeListener();
        await options.client.projectionUnsubscribe(activeSubscriptionId);
      }
    };
  } catch (error) {
    removeListener();
    if (subscriptionId != null) {
      await options.client.projectionUnsubscribe(subscriptionId).catch(() => undefined);
    }
    throw error;
  } finally {
    clearTimeout(timeoutId);
  }
}

export interface ProjectionWatch {
  unsubscribe: () => Promise<void>;
  resync: () => Promise<void>;
}

/** Maintains one materialised projection through disconnects and resynchronisation. */
export async function watchProjection<
  M extends ProjectionModel,
  TProjection extends ProjectionName<M>
>(options: {
  client: ProjectionTransport<M>;
  plans: ProjectionCompositionPlans<ProjectionName<M>>;
  signal?: AbortSignal;
  initialSnapshotTimeoutMs?: number;
  projection: TProjection;
  params: ProjectionParams<M, TProjection>;
  onValue: (
    snapshot: ProjectionSnapshot<M, TProjection>,
    patch?: ProjectionPatch<M, TProjection>
  ) => void;
  onError: (error: unknown) => void;
}): Promise<ProjectionWatch> {
  let resolveFirst!: () => void;
  let rejectFirst!: (error: unknown) => void;
  const firstSnapshot = new Promise<void>((resolve, reject) => { resolveFirst = resolve; rejectFirst = reject; });
  const timeout = setTimeout(
    () => rejectFirst(new Error("Timed out waiting for projection snapshot.")),
    options.initialSnapshotTimeoutMs ?? 30_000
  );
  let disposed = false;
  let generation = 0;
  let subscriptionId: number | null = null;
  let connecting: Promise<void> | null = null;
  let resyncing = false;
  const earlyEvents: ProjectionTransportEvent<M>[] = [];
  let state = createProjectionMaterializerState<M>(options.plans);
  const receive = (event: ProjectionTransportEvent<M>): void => {
    if (event.subscription_id !== subscriptionId) return;
    const previous = state.subscriptions.get(event.subscription_id);
    const revision = event.kind === "snapshot" ? event.revision : event.to_revision;
    if (previous && revision <= previous.value.revision) return;
    try {
      const update = applyProjectionEvent(state, event);
      if (update.projection !== options.projection) throw new Error("Projection subscription changed its contract.");
      const snapshot = update.value.snapshot as ProjectionSnapshot<M, TProjection>;
      options.onValue(snapshot, event.kind === "patch" ? event.patch.value as ProjectionPatch<M, TProjection> : undefined);
      resolveFirst();
    } catch (error) {
      options.onError(error);
      if (subscriptionId !== null && !resyncing) {
        resyncing = true;
        void options.client.projectionResync(subscriptionId).catch(options.onError).finally(() => { resyncing = false; });
      }
    }
  };
  const removeListener = options.client.addNotificationListener((notification) => {
    if (disposed || notification.method !== PROJECTION_UPDATE_NOTIFICATION || !isProjectionTransportEvent<M>(notification.params)) return;
    const event = notification.params;
    if ((event.kind === "snapshot" ? event.snapshot.projection : event.patch.projection) !== options.projection) return;
    if (subscriptionId === null) {
      earlyEvents.push(event);
    } else receive(event);
  });
  const connect = (): Promise<void> => {
    if (disposed || subscriptionId !== null) return Promise.resolve();
    if (connecting) return connecting;
    const current = generation;
    connecting = Promise.resolve().then(async () => {
      const accepted = await options.client.projectionSubscribe(options.projection, options.params);
      // A previous connection owns its subscription ids; they may be reused after reconnect.
      if (current !== generation) return;
      if (disposed) {
        await options.client.projectionUnsubscribe(accepted.subscription_id).catch(options.onError);
        return;
      }
      subscriptionId = accepted.subscription_id;
      for (const event of earlyEvents.splice(0)) receive(event);
    }).finally(() => { if (current === generation) connecting = null; });
    return connecting;
  };
  const removeConnectionListener = options.client.addConnectionStateListener((connection) => {
    if (connection.status === "connected") {
      void connect().catch(error => { options.onError(error); if (options.client.isRefusal(error)) rejectFirst(error); });
    } else if (connection.status !== "connecting") {
      generation += 1;
      subscriptionId = null;
      connecting = null;
      resyncing = false;
      state = createProjectionMaterializerState<M>(options.plans);
      earlyEvents.length = 0;
    }
  });
  const abort = (): void => {
    disposed = true;
    removeListener();
    removeConnectionListener();
    rejectFirst(new Error("Projection subscription cancelled."));
    if (subscriptionId !== null) {
      const id = subscriptionId;
      subscriptionId = null;
      void options.client.projectionUnsubscribe(id).catch(options.onError);
    }
  };
  options.signal?.addEventListener("abort", abort, { once: true });
  if (options.signal?.aborted) abort();
  else void connect().catch(error => { if (!disposed) { options.onError(error); if (options.client.isRefusal(error)) rejectFirst(error); } });
  try { await firstSnapshot; } catch (error) {
    disposed = true;
    options.signal?.removeEventListener("abort", abort);
    removeListener();
    removeConnectionListener();
    if (subscriptionId !== null) await options.client.projectionUnsubscribe(subscriptionId).catch(options.onError);
    throw error;
  } finally { clearTimeout(timeout); }
  return { resync: async () => {
    await connect();
    if (subscriptionId === null) throw new Error("The projection is disconnected.");
    await options.client.projectionResync(subscriptionId);
  }, unsubscribe: async () => {
    disposed = true;
    options.signal?.removeEventListener("abort", abort);
    removeListener();
    removeConnectionListener();
    if (subscriptionId !== null) await options.client.projectionUnsubscribe(subscriptionId);
  } };
}
