/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Replica helpers shared by the Loro adapter tests.
 */
import type { CollaborationPlanTextFieldPath } from "@clerkenwell/client";
import {
  CollaborationDrafts,
  CollaborationLoroAuthoringDocument,
  documentDraftMapping
} from "@clerkenwell/client/loro";

import {
  BOARD_PLAN,
  NOTE_PLAN,
  type BoardDocument,
  type NoteDocument
} from "./plans";

export type NoteReplica = CollaborationLoroAuthoringDocument<NoteDocument>;
export type BoardReplica = CollaborationLoroAuthoringDocument<BoardDocument>;

/** A board's declared text fields. */
export type BoardTextFieldPath = CollaborationPlanTextFieldPath<typeof BOARD_PLAN>;

/** Board replicas drafted as the whole board, as an authoring runtime drives them. */
export const BOARD_DRAFTS = new CollaborationDrafts(
  "Board",
  BOARD_PLAN,
  documentDraftMapping<BoardDocument>()
);

export function noteReplica(document: NoteDocument): NoteReplica {
  return new CollaborationLoroAuthoringDocument("Note", NOTE_PLAN, { kind: "document", document });
}

export function noteReplicaFromUpdate(updateBase64: string): NoteReplica {
  return new CollaborationLoroAuthoringDocument<NoteDocument>("Note", NOTE_PLAN, {
    kind: "update",
    updateBase64
  });
}

export function boardReplica(document: BoardDocument): BoardReplica {
  return new CollaborationLoroAuthoringDocument("Board", BOARD_PLAN, { kind: "document", document });
}

export function boardReplicaFromUpdate(updateBase64: string): BoardReplica {
  return new CollaborationLoroAuthoringDocument<BoardDocument>("Board", BOARD_PLAN, {
    kind: "update",
    updateBase64
  });
}

/** What a document reads as once it has been written to a replica and read back from its update. */
export function roundTripNote(document: NoteDocument): NoteDocument {
  return noteReplicaFromUpdate(noteReplica(document).exportUpdateBase64()).currentDocument();
}

export function roundTripBoard(document: BoardDocument): BoardDocument {
  return boardReplicaFromUpdate(boardReplica(document).exportUpdateBase64()).currentDocument();
}

/**
 * Seeds `base`, lets each writer replace the whole document from it
 * independently, and returns what replicas that received every write hold,
 * once for each import order.
 */
export function mergeRewrites<TDocument extends object>(
  seed: CollaborationLoroAuthoringDocument<TDocument>,
  rewrites: readonly [(document: TDocument) => TDocument, (document: TDocument) => TDocument]
): { forward: TDocument; reverse: TDocument } {
  const frontier = seed.acceptedFrontierBase64();
  const updates = rewrites.map((rewrite) => {
    const writer = seed.fork();
    writer.replaceDocument(rewrite(writer.currentDocument()));
    return writer.exportIncrementalUpdateBase64(frontier);
  });
  const forward = seed.fork();
  const reverse = seed.fork();
  for (const update of updates) forward.importUpdateBase64(update);
  for (const update of [...updates].reverse()) reverse.importUpdateBase64(update);
  return { forward: forward.currentDocument(), reverse: reverse.currentDocument() };
}
