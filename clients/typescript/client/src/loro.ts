/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Plan-driven replicas over Loro, loaded only by applications that edit
 * collaborative documents. Only what this file names is public.
 */

export type {
  CollaborationDraftMapping,
  CollaborationReplicaSource,
  CollaborationReplicaView
} from "./collaboration-loro";
export {
  CollaborationDraftReplica,
  CollaborationDrafts,
  CollaborationLoroAuthoringDocument,
  CollaborationTextView,
  collaborationReplicaSource,
  documentDraftMapping,
  requireCollaborationSchemaVersion
} from "./collaboration-loro";
