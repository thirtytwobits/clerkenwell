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
  AuthoringSessionHandle
} from "@clerkenwell/client";

import { BOARD_PLAN, type BoardDocument } from "./plans";
import {
  BOARD_DRAFTS,
  boardReplica,
  type BoardReplica,
  type BoardTextFieldPath
} from "./replicas";

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

  /**
   * Imports an update whose base frontier the server already holds. The
   * importer lacks what the server held beyond that frontier.
   */
  accept(outgoing: BoardAuthoringUpdate): {
    state: BoardAuthoringState;
    board: BoardDocument;
    missingUpdateBase64: string;
  } {
    if (!this.accepted.coversFrontierBase64(outgoing.baseFrontierBase64)) {
      throw new Error("Unknown base frontier.");
    }
    this.imports.push(outgoing);
    const missingUpdateBase64 = this.accepted.exportIncrementalUpdateBase64(outgoing.baseFrontierBase64);
    this.accepted.importUpdateBase64(outgoing.updateBase64);
    return { state: this.snapshot(), board: this.board(), missingUpdateBase64 };
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

/** Opens a clean session on the server's accepted state with a live replica behind it. */
export function openBoardSession(
  runtime: AuthoringRuntime,
  server: FakeBoardServer,
  resource: AuthoringResourceIdentity
): AuthoringSessionHandle<BoardDocument, BoardTextFieldPath> {
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
    BOARD_DRAFTS.fromUpdate(initial.update_base64).controller());
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
