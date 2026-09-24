/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * TypeScript replicas for the Rust conformance driver in
 * `crates/clerkenwell-conformance`. The bridge is started with one
 * definition's generated TypeScript plans and its collaboration fixture
 * corpus, holds named replicas of `@clerkenwell/client/loro`, and answers each
 * JSON request line on standard input with one JSON response line on standard
 * output: `{"ok": true, "value": ...}` or `{"ok": false, "error": "..."}`.
 */
import { readFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

import type { CollaborationEntityPlan, TextBinding } from "@clerkenwell/client";
import {
  CollaborationLoroAuthoringDocument,
  requireCollaborationSchemaVersion
} from "@clerkenwell/client/loro";

type Identities = Readonly<Record<string, string>>;

type Request =
  | { readonly op: "seedFixture"; readonly replica: string; readonly entity: string }
  | { readonly op: "hydrate"; readonly replica: string; readonly entity: string; readonly schemaVersion: number; readonly updateBase64: string }
  | { readonly op: "replace"; readonly replica: string; readonly document: object }
  | { readonly op: "import"; readonly replica: string; readonly schemaVersion: number; readonly updateBase64: string }
  | { readonly op: "export"; readonly replica: string }
  | { readonly op: "exportIncremental"; readonly replica: string; readonly frontierBase64: string }
  | { readonly op: "frontier"; readonly replica: string }
  | { readonly op: "materialize"; readonly replica: string; readonly revision: string }
  | { readonly op: "captureText"; readonly replica: string; readonly field: string; readonly identities: Identities }
  | { readonly op: "readText"; readonly replica: string; readonly field: string; readonly identities: Identities }
  | { readonly op: "insertText"; readonly replica: string; readonly field: string; readonly identities: Identities; readonly offset: number; readonly text: string };

interface CollaborationFixtures {
  readonly entities: readonly { readonly entity: string; readonly clientDocument: object }[];
}

type Replica = CollaborationLoroAuthoringDocument<object>;

const { values: options } = parseArgs({
  options: {
    plans: { type: "string" },
    fixtures: { type: "string" }
  }
});
if (options.plans === undefined || options.fixtures === undefined) {
  throw new Error("Usage: bridge.ts --plans <generated TypeScript model> --fixtures <collaboration fixture corpus>");
}
const plansModule = await import(pathToFileURL(options.plans).href) as {
  readonly COLLABORATION_PLANS?: Readonly<Record<string, CollaborationEntityPlan>>;
};
const plans = plansModule.COLLABORATION_PLANS;
if (plans === undefined) {
  throw new Error(`${options.plans} does not export COLLABORATION_PLANS.`);
}
const fixtures = JSON.parse(await readFile(options.fixtures, "utf8")) as CollaborationFixtures;
const replicas = new Map<string, Replica>();

function plan(entity: string): CollaborationEntityPlan {
  const found = plans?.[entity];
  if (found === undefined) {
    throw new Error(`The plans declare no collaborative entity ${entity}.`);
  }
  return found;
}

function replica(name: string): Replica {
  const found = replicas.get(name);
  if (found === undefined) {
    throw new Error(`No replica is named ${name}.`);
  }
  return found;
}

function create(name: string, created: Replica): null {
  if (replicas.has(name)) {
    throw new Error(`A replica is already named ${name}.`);
  }
  replicas.set(name, created);
  return null;
}

function text(request: { readonly replica: string; readonly field: string; readonly identities: Identities }): TextBinding {
  return replica(request.replica).bindText(request.field, request.identities);
}

function handle(request: Request): unknown {
  switch (request.op) {
    case "seedFixture": {
      const fixture = fixtures.entities.find(({ entity }) => entity === request.entity);
      if (fixture === undefined) {
        throw new Error(`The fixture corpus has no ${request.entity} entity.`);
      }
      return create(request.replica, new CollaborationLoroAuthoringDocument(
        request.entity,
        plan(request.entity),
        { kind: "document", document: fixture.clientDocument }
      ));
    }
    case "hydrate": {
      const entityPlan = plan(request.entity);
      requireCollaborationSchemaVersion(request.entity, entityPlan, request.schemaVersion);
      return create(request.replica, new CollaborationLoroAuthoringDocument(
        request.entity,
        entityPlan,
        { kind: "update", updateBase64: request.updateBase64 }
      ));
    }
    case "replace":
      replica(request.replica).replaceDocument(request.document);
      return null;
    case "import":
      replica(request.replica).importVersionedUpdateBase64(request.schemaVersion, request.updateBase64);
      return null;
    case "export":
      return replica(request.replica).exportUpdateBase64();
    case "exportIncremental":
      return replica(request.replica).exportIncrementalUpdateBase64(request.frontierBase64);
    case "frontier":
      return replica(request.replica).acceptedFrontierBase64();
    case "materialize":
      return replica(request.replica).materializedDocument(request.revision);
    case "captureText":
      return {
        text: text(request).read(),
        frontierBase64: replica(request.replica).acceptedFrontierBase64()
      };
    case "readText":
      return text(request).read();
    case "insertText": {
      const binding = text(request);
      const end = request.offset + request.text.length;
      binding.edit({
        baseRevision: binding.revision,
        changes: [{ from: request.offset, to: request.offset, insert: request.text }],
        selectionBefore: { anchor: request.offset, head: request.offset },
        selectionAfter: { anchor: end, head: end },
        group: "conformance-typing"
      });
      return null;
    }
    default:
      throw new Error(`Unknown bridge operation ${(request as { readonly op: string }).op}.`);
  }
}

for await (const line of createInterface({ input: process.stdin, crlfDelay: Infinity })) {
  if (line.trim().length === 0) continue;
  let response: object;
  try {
    response = { ok: true, value: handle(JSON.parse(line) as Request) ?? null };
  } catch (error) {
    response = { ok: false, error: error instanceof Error ? error.message : String(error) };
  }
  process.stdout.write(`${JSON.stringify(response)}\n`);
}
