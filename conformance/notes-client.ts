/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Two TypeScript clients of the Rust notes server, for the conformance driver
 * in `crates/clerkenwell-conformance`. Ada creates a note and watches it;
 * Grace writes `--body` into it from her own replica. The script prints, as
 * one JSON line, the note Ada's watch delivers once it holds that body.
 *
 * With `--reconnect`, Ada's connection drops once her watch holds the note,
 * and a line before the note reports how her watch resubscribed: whether the
 * server found her up to date, and how many snapshots it sent her.
 */
import { parseArgs } from "node:util";

import {
  ProjectionClient,
  RpcSocket,
  subscribeProjection,
  watchProjection
} from "@clerkenwell/client";
import { CollaborationLoroAuthoringDocument } from "@clerkenwell/client/loro";

import {
  COLLABORATION_PLANS,
  PROJECTION_COMPOSITION_PLANS,
  type GeneratedProjectionModel
} from "../examples/notes/generated/typescript/index";

type Model = GeneratedProjectionModel;
type Note = Record<string, unknown>;

const { values } = parseArgs({
  options: { url: { type: "string" }, body: { type: "string" }, reconnect: { type: "boolean" } }
});
if (values.url === undefined || values.body === undefined) {
  throw new Error("notes-client needs --url and --body.");
}
const { url, body } = values as { url: string; body: string };

function client(): ProjectionClient<Model> {
  return new ProjectionClient<Model>(new RpcSocket({ url, reconnectOnClose: false }));
}

function replica(updateBase64: string): CollaborationLoroAuthoringDocument<Note> {
  return CollaborationLoroAuthoringDocument.from<Note>("Note", COLLABORATION_PLANS.Note, {
    kind: "update",
    updateBase64
  });
}

const ada = client();
const grace = client();
await Promise.all([ada.socket.connect(), grace.socket.connect()]);
const { note_id } = await ada.projectionMutate("note.create", { title: "Launch plan" });

let delivered!: (note: Note) => void;
let failed!: (error: unknown) => void;
const written = new Promise<Note>((resolve, reject) => { delivered = resolve; failed = reject; });
let held: CollaborationLoroAuthoringDocument<Note> | undefined;
const watch = await watchProjection<Model, "notes.authoringState">({
  client: ada,
  plans: PROJECTION_COMPOSITION_PLANS,
  projection: "notes.authoringState",
  params: { note_id },
  // The first delivery carries every accepted operation; each later one, those Ada lacked.
  onValue: (state) => {
    if (held === undefined) held = replica(state.update_base64);
    else held.importUpdateBase64(state.update_base64);
    const note = held.currentDocument();
    if (note.body === body) delivered(note);
  },
  onError: failed
});

if (values.reconnect === true) {
  let snapshots = 0;
  ada.addNotificationListener((notification) => {
    const event = notification.params as { kind?: string } | undefined;
    if (notification.method === "projection.update" && event?.kind === "snapshot") snapshots += 1;
  });
  const subscribe = ada.projectionSubscribe.bind(ada);
  const resubscribed = new Promise<{ up_to_date?: boolean }>((resolve) => {
    ada.projectionSubscribe = async (...args: Parameters<typeof subscribe>) => {
      const accepted = await subscribe(...args);
      resolve(accepted);
      return accepted;
    };
  });
  await ada.socket.disconnect();
  await ada.socket.connect();
  const accepted = await resubscribed;
  console.log(JSON.stringify({ up_to_date: accepted.up_to_date === true, snapshots }));
}

const read = await subscribeProjection<Model, "notes.authoringState">({
  client: grace,
  projection: "notes.authoringState",
  params: { note_id }
});
const edited = replica(read.snapshot.update_base64);
edited.replaceDocument({ ...edited.currentDocument(), body });
await grace.projectionMutate("note.importLoroUpdate", {
  note_id,
  operation_id: "grace-body",
  exchange_mode: "incremental",
  base_frontier_base64: read.snapshot.accepted_frontier_base64,
  update_base64: edited.exportIncrementalUpdateBase64(read.snapshot.accepted_frontier_base64)
});

console.log(JSON.stringify(await written));
await Promise.all([watch.unsubscribe(), read.unsubscribe()]);
await Promise.all([ada.socket.disconnect(), grace.socket.disconnect()]);
