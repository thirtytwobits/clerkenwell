/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Plan-driven replicas over Loro, loaded only by applications that edit
 * collaborative documents. Only what this file names is public.
 */

export type {
  CollaborationDraftMapping,
  CollaborationPeerBlock,
  CollaborationReplicaSource,
  CollaborationReplicaView
} from "./collaboration-replica.js";
export {
  CollaborationDraftReplica,
  CollaborationDrafts,
  CollaborationReplica,
  CollaborationTextView,
  collaborationReplicaSource,
  documentDraftMapping,
  requireCollaborationSchemaVersion
} from "./collaboration-replica.js";
