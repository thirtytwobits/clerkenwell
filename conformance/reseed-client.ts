/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A TypeScript client of the Rust notes server whose note is re-seeded behind
 * it, for the conformance driver in `crates/clerkenwell-conformance`. A new
 * server numbers its notes from the first again, so the same note on a new
 * server is a re-seed: one identity, an unrelated history.
 *
 * With `--hold`, Grace creates a note, takes it into a replica and writes
 * `--body` into it without sending it. The script prints, as one JSON line,
 * what she holds.
 *
 * With `--resume <that line>`, against a new server, Grace creates the note
 * again and resumes from what she held. The script prints, as one JSON line,
 * whether the server said her state replaces what she held, whether her old
 * replica refused the server's history, the kind of conflict the server refused her old
 * history with, and the etag her edit, written again on a new replica, was
 * accepted at.
 */
import { parseArgs } from "node:util";

import { ProjectionClient, RpcError, RpcSocket, subscribeProjection } from "@clerkenwell/client";
import {
  CollaborationReplica,
  CollaborationUnrelatedHistoryError
} from "@clerkenwell/client/replica";

import {
  COLLABORATION_PLANS,
  type AuthoringState,
  type GeneratedProjectionModel
} from "../examples/notes/generated/typescript/index.js";

type Model = GeneratedProjectionModel;
type Note = Record<string, unknown>;

interface Held {
  readonly note_id: string;
  readonly held: { schema_version: number; frontier: string; etag: string };
  readonly update_base64: string;
  readonly peer_nonces: string[];
  readonly body: string;
}

const { values } = parseArgs({
  options: {
    url: { type: "string" },
    body: { type: "string" },
    hold: { type: "boolean" },
    resume: { type: "string" }
  }
});
if (values.url === undefined) {
  throw new Error("reseed-client needs --url.");
}
const url = values.url;

const grace = new ProjectionClient<Model>(
  new RpcSocket({ url: `${url}?writer=grace`, reconnectOnClose: false })
);
await grace.socket.connect();
const { note_id } = await grace.projectionMutate("note.create", { title: "Launch plan" });

function replica(state: AuthoringState): CollaborationReplica<Note> {
  return CollaborationReplica.from<Note>(
    "Note",
    COLLABORATION_PLANS.Note,
    { kind: "update", updateBase64: state.update_base64 },
    state.peer_block
  );
}

/** The authoring state the server sends a subscription naming `held`. */
function resume(held: unknown): Promise<AuthoringState> {
  return new Promise((resolve, reject) => {
    grace.addNotificationListener((notification) => {
      const event = notification.params as {
        kind?: string;
        snapshot?: { value?: AuthoringState };
      } | undefined;
      if (
        notification.method === "projection.update"
        && event?.kind === "snapshot"
        && event.snapshot?.value !== undefined
      ) {
        resolve(event.snapshot.value);
      }
    });
    grace.projectionSubscribe("notes.authoringState", { note_id }, { held }).catch(reject);
  });
}

/** The kind of conflict a refusal names, as the store's refusals carry it. */
function conflictKind(error: unknown): unknown {
  if (!(error instanceof RpcError)) throw error;
  return (error.data as { conflict_kind?: unknown } | undefined)?.conflict_kind;
}

if (values.hold === true) {
  if (values.body === undefined) throw new Error("--hold needs --body.");
  const read = await subscribeProjection<Model, "notes.authoringState">({
    client: grace,
    projection: "notes.authoringState",
    params: { note_id }
  });
  const offline = replica(read.snapshot);
  offline.replaceDocument({ ...offline.currentDocument(), body: values.body });
  const held: Held = {
    note_id,
    held: {
      schema_version: read.snapshot.schema_version,
      frontier: read.snapshot.accepted_frontier_base64,
      etag: read.snapshot.etag
    },
    update_base64: offline.exportUpdateBase64(),
    peer_nonces: [...offline.peerNonces()],
    body: values.body
  };
  console.log(JSON.stringify(held));
} else if (values.resume !== undefined) {
  const held = JSON.parse(values.resume) as Held;
  if (held.note_id !== note_id) {
    throw new Error(`The new server named the note ${note_id}, not ${held.note_id}.`);
  }
  const state = await resume(held.held);

  const old = replica({ ...state, update_base64: held.update_base64 });
  old.includePeerNonces(held.peer_nonces);
  let refusedServersHistory = false;
  try {
    old.importUpdateBase64(state.update_base64);
  } catch (error) {
    refusedServersHistory = error instanceof CollaborationUnrelatedHistoryError;
  }

  let oldHistoryRefusal: unknown = null;
  try {
    await grace.projectionMutate("note.importUpdate", {
      note_id,
      operation_id: "old-history",
      exchange_mode: "incremental",
      base_frontier_base64: "",
      update_base64: old.exportUpdateBase64(),
      peer_nonces: [...old.peerNonces()]
    });
  } catch (error) {
    oldHistoryRefusal = conflictKind(error);
  }

  const fresh = replica(state);
  fresh.replaceDocument({ ...fresh.currentDocument(), body: held.body });
  const accepted = await grace.projectionMutate("note.importUpdate", {
    note_id,
    operation_id: "written-again",
    exchange_mode: "incremental",
    base_frontier_base64: state.accepted_frontier_base64,
    update_base64: fresh.exportIncrementalUpdateBase64(state.accepted_frontier_base64),
    peer_nonces: [...fresh.peerNonces()]
  });

  console.log(JSON.stringify({
    replaces_held: state.replaces_held,
    refused_servers_history: refusedServersHistory,
    old_history_refusal: oldHistoryRefusal,
    accepted_etag: accepted.etag
  }));
} else {
  throw new Error("reseed-client needs --hold or --resume.");
}

await grace.socket.disconnect();
