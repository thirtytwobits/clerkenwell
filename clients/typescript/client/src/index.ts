/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The React-free client. It never loads the CRDT runtime; replicas come from
 * `@clerkenwell/client/replica`. Only what this file names is public.
 */

// The authoring runtime, its sessions and their controllers.
export type {
  AuthoringDraftOperations,
  AuthoringExchangeMode,
  AuthoringResourceIdentity,
  AuthoringRuntimeListener,
  AuthoringRuntimeState,
  AuthoringSession,
  AuthoringSessionController,
  AuthoringSessionStatus,
  AuthoringTextStageConfirmation,
  AuthoringTextStageController,
  QueuedAuthoringOperation,
  RedactedAuthoringRuntimeDiagnostic,
  RedactedAuthoringSessionDiagnostic
} from "./authoring-session.js";
export { AuthoringRuntime, redactedAuthoringRuntimeDiagnostic } from "./authoring-runtime.js";
export { authoringSessionAcceptsDraft } from "./authoring-state-machine.js";

// What an application persists of the runtime.
export type {
  PersistedAuthoringDraftOperations,
  PersistedAuthoringResourceIdentity,
  PersistedAuthoringRuntimeState,
  PersistedAuthoringSessionState,
  PersistedQueuedAuthoringOperation
} from "./authoring-persistence.js";
export {
  durableSessions,
  fromPersistedRuntime,
  sameSessions,
  toPersistedRuntime
} from "./authoring-persistence.js";

// Plans, as generated bindings instantiate them.
export type {
  AuthoringPlan,
  AuthoringPolicyKind,
  CollaborationConflictPolicy,
  CollaborationEntityPlan,
  CollaborationFieldPlan,
  CollaborationFieldStorage,
  CollaborationPlanTextFieldPath,
  CollaborationStorageKind,
  CollaborationValueCodec,
  CollaborationWriterKind,
  ProjectionCompositionPlan,
  ProjectionCompositionPlans
} from "./plans.js";
export { resolveCollaborationContainer } from "./plans.js";
export { conflictingFieldPaths, creationConflictingFieldPaths } from "./conflict-policy.js";

// A projection model's names, parameters and payloads.
export type {
  MutationContract,
  MutationName,
  MutationParams,
  MutationResult,
  ProjectionContract,
  ProjectionModel,
  ProjectionName,
  ProjectionParams,
  ProjectionPatch,
  ProjectionSnapshot,
  ProjectionTransportMutationResult,
  ProjectionTransportPatch,
  ProjectionTransportSnapshot
} from "./projection-model.js";

// The session protocol on the wire.
export type {
  ProjectionErrorCode,
  ProjectionErrorEnvelope,
  ProjectionMutationAccepted,
  ProjectionMutationCommand,
  ProjectionResyncAccepted,
  ProjectionResyncCommand,
  ProjectionSubscribeAccepted,
  ProjectionSubscribeCommand,
  ProjectionTransportEvent,
  ProjectionUnsubscribeAccepted,
  ProjectionUnsubscribeCommand
} from "./protocol.js";
export {
  PROJECTION_MUTATE_METHOD,
  PROJECTION_RESYNC_METHOD,
  PROJECTION_SUBSCRIBE_METHOD,
  PROJECTION_UNSUBSCRIBE_METHOD,
  PROJECTION_UPDATE_NOTIFICATION,
  isProjectionTransportEvent
} from "./protocol.js";

// The socket, the projection client, subscription and materialisation.
export type {
  RpcNotification,
  RpcSocketOptions,
  RpcSocketState,
  RpcSocketStatus,
  WebSocketLike
} from "./rpc-socket.js";
export { RpcError, RpcSocket } from "./rpc-socket.js";
export { ProjectionClient } from "./projection-client.js";
export type {
  ProjectionConnectionState,
  ProjectionNotification,
  ProjectionSubscribeTransport,
  ProjectionSubscription,
  ProjectionTransport,
  ProjectionWatch
} from "./projection-subscription.js";
export { subscribeProjection, watchProjection } from "./projection-subscription.js";
export type {
  MaterializedProjection,
  MaterializedProjectionFor,
  MaterializedProjectionValue,
  ProjectionMaterializerState
} from "./materialize.js";
export {
  ProjectionMaterializerError,
  applyProjectionEvent,
  createProjectionMaterializerState
} from "./materialize.js";

// Text binding.
export type { TextBinding, TextBindingChange, TextEdit, TextSelection } from "./text-binding.js";
export { validateTextEdits } from "./text-binding.js";
