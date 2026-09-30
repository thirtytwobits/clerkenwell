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
} from "./authoring-session";
export { AuthoringRuntime, redactedAuthoringRuntimeDiagnostic } from "./authoring-runtime";
export { authoringSessionAcceptsDraft } from "./authoring-state-machine";

// What an application persists of the runtime.
export type {
  PersistedAuthoringDraftOperations,
  PersistedAuthoringResourceIdentity,
  PersistedAuthoringRuntimeState,
  PersistedAuthoringSessionState,
  PersistedQueuedAuthoringOperation
} from "./authoring-persistence";
export {
  durableSessions,
  fromPersistedRuntime,
  sameSessions,
  toPersistedRuntime
} from "./authoring-persistence";

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
  ProjectionCompositionPlan,
  ProjectionCompositionPlans
} from "./plans";
export { resolveCollaborationContainer } from "./plans";
export { conflictingFieldPaths } from "./conflict-policy";

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
} from "./projection-model";

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
} from "./protocol";
export {
  PROJECTION_MUTATE_METHOD,
  PROJECTION_RESYNC_METHOD,
  PROJECTION_SUBSCRIBE_METHOD,
  PROJECTION_UNSUBSCRIBE_METHOD,
  PROJECTION_UPDATE_NOTIFICATION,
  isProjectionTransportEvent
} from "./protocol";

// The socket, the projection client, subscription and materialisation.
export type {
  RpcNotification,
  RpcSocketOptions,
  RpcSocketState,
  RpcSocketStatus,
  WebSocketLike
} from "./rpc-socket";
export { RpcError, RpcSocket } from "./rpc-socket";
export { ProjectionClient } from "./projection-client";
export type {
  ProjectionConnectionState,
  ProjectionNotification,
  ProjectionSubscribeTransport,
  ProjectionSubscription,
  ProjectionTransport,
  ProjectionWatch
} from "./projection-subscription";
export { subscribeProjection, watchProjection } from "./projection-subscription";
export type {
  MaterializedProjection,
  MaterializedProjectionFor,
  MaterializedProjectionValue,
  ProjectionMaterializerState
} from "./materialize";
export {
  ProjectionMaterializerError,
  applyProjectionEvent,
  createProjectionMaterializerState
} from "./materialize";

// Text binding.
export type { TextBinding, TextBindingChange, TextEdit, TextSelection } from "./text-binding";
export { validateTextEdits } from "./text-binding";
