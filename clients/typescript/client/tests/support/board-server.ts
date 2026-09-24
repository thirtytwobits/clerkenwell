/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A board server stand-in for authoring tests: it holds the accepted document,
 * publishes it as an authoring-state snapshot, accepts incremental updates
 * against a frontier it recognises, and lets another writer change the
 * document behind the editor's back.
 */
import type {
  AuthoringResourceIdentity,
  AuthoringRuntime,
  AuthoringSessionController,
  AuthoringSessionHandle
} from "@clerkenwell/client";

import { BOARD_PLAN, type BoardDocument } from "./plans";
import { boardReplica, boardReplicaFromUpdate, type BoardReplica } from "./replicas";

export interface BoardAuthoringState {
  schema_version: number;
  accepted_frontier_base64: string;
  update_base64: string;
  exchange_modes: ("incremental" | "bootstrap")[];
}

export interface BoardAuthoringUpdate {
  baseFrontierBase64: string;
  updateBase64: string;
}

export class FakeBoardServer {
  private readonly accepted: BoardReplica;
  /** Every incremental update the server was asked to import. */
  readonly imports: BoardAuthoringUpdate[] = [];

  constructor(board: BoardDocument) {
    this.accepted = boardReplica(board);
  }

  board(): BoardDocument {
    return this.accepted.currentDocument();
  }

  snapshot(): BoardAuthoringState {
    return {
      schema_version: BOARD_PLAN.schemaVersion,
      accepted_frontier_base64: this.accepted.acceptedFrontierBase64(),
      update_base64: this.accepted.exportUpdateBase64(),
      exchange_modes: ["incremental", "bootstrap"]
    };
  }

  /** Imports an update whose base frontier the server already holds. */
  accept(outgoing: BoardAuthoringUpdate): { state: BoardAuthoringState; board: BoardDocument } {
    if (!this.accepted.coversFrontierBase64(outgoing.baseFrontierBase64)) {
      throw new Error("Unknown base frontier.");
    }
    this.imports.push(outgoing);
    this.accepted.importUpdateBase64(outgoing.updateBase64);
    return { state: this.snapshot(), board: this.board() };
  }

  /** Another writer's change, accepted by the server. */
  editRemotely(mutate: (board: BoardDocument) => BoardDocument): BoardAuthoringState {
    const base = this.accepted.acceptedFrontierBase64();
    const peer = this.accepted.fork();
    peer.replaceDocument(mutate(peer.currentDocument()));
    this.accepted.importUpdateBase64(peer.exportIncrementalUpdateBase64(base));
    return this.snapshot();
  }
}

export function boardAuthoringController(
  replica: BoardReplica
): AuthoringSessionController<BoardDocument> {
  return {
    currentDraft: () => replica.currentDocument(),
    replaceDraft: (board) => replica.replaceDocument(board),
    adoptDocument: (board) => replica.adoptDocument(board),
    bindText: (fieldPath, identities) => replica.bindText(fieldPath, identities),
    stageText: (fieldPath, identities) => replica.stageText(fieldPath, identities),
    importUpdateBase64: (updateBase64) => { replica.importUpdateBase64(updateBase64); },
    importVersionedUpdateBase64: (schemaVersion, updateBase64) =>
      replica.importVersionedUpdateBase64(schemaVersion, updateBase64),
    exportUpdateBase64: () => replica.exportUpdateBase64(),
    exportIncrementalUpdateBase64: (frontier) => replica.exportIncrementalUpdateBase64(frontier),
    acceptedFrontierBase64: () => replica.acceptedFrontierBase64(),
    coversFrontierBase64: (frontier) => replica.coversFrontierBase64(frontier),
    dispose: () => replica.disposeTextBindings()
  };
}

/** Opens a clean session on the server's accepted state with a live replica behind it. */
export function openBoardSession(
  runtime: AuthoringRuntime,
  server: FakeBoardServer,
  resource: AuthoringResourceIdentity
): AuthoringSessionHandle<BoardDocument> {
  const initial = server.snapshot();
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: initial.schema_version,
    acceptedRevision: initial.accepted_frontier_base64,
    supportedExchangeModes: [...initial.exchange_modes],
    baseline: server.board(),
    draft: server.board()
  });
  return runtime.ensureController(resource, () =>
    boardAuthoringController(boardReplicaFromUpdate(initial.update_base64)));
}

export function renameTask(board: BoardDocument, taskId: string, title: string): BoardDocument {
  const next = structuredClone(board);
  const task = next.columns.flatMap(({ tasks }) => tasks).find(({ id }) => id === taskId);
  if (task === undefined) {
    throw new Error(`No task ${taskId}.`);
  }
  task.title = title;
  return next;
}

export function relabelTask(board: BoardDocument, taskId: string, labels: string[]): BoardDocument {
  const next = structuredClone(board);
  const task = next.columns.flatMap(({ tasks }) => tasks).find(({ id }) => id === taskId);
  if (task === undefined) {
    throw new Error(`No task ${taskId}.`);
  }
  task.labels = labels;
  return next;
}

export function addTask(board: BoardDocument, columnId: string, taskId: string): BoardDocument {
  const next = structuredClone(board);
  const column = next.columns.find((candidate) => candidate.columnId === columnId);
  if (column === undefined) {
    throw new Error(`No column ${columnId}.`);
  }
  column.tasks.push({ id: taskId, title: taskId, notes: "", labels: [] });
  return next;
}
