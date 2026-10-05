/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * TypeScript replicas for the Rust conformance driver in
 * `crates/clerkenwell-conformance`, which writes this file into the npm project
 * whose `@clerkenwell/client` it drives. The bridge is started with one
 * definition's generated TypeScript plans and its collaboration fixture
 * corpus, holds named replicas of `@clerkenwell/client/replica`, and answers each
 * JSON request line on standard input with one JSON response line on standard
 * output: `{"ok": true, "value": ...}` or `{"ok": false, "error": "..."}`.
 */
import { existsSync, readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

import {
  conflictingFieldPaths,
  type CollaborationEntityPlan,
  type CollaborationWriterKind,
  type TextBinding
} from "@clerkenwell/client";
import {
  CollaborationReplica,
  requireCollaborationSchemaVersion,
  type CollaborationPeerBlock
} from "@clerkenwell/client/replica";

type Identities = Readonly<Record<string, string>>;

type Request =
  | { readonly op: "clientVersion" }
  | { readonly op: "seedFixture"; readonly replica: string; readonly entity: string; readonly peerBlock: CollaborationPeerBlock }
  | { readonly op: "hydrate"; readonly replica: string; readonly entity: string; readonly schemaVersion: number; readonly updateBase64: string; readonly peerBlock: CollaborationPeerBlock }
  | { readonly op: "replace"; readonly replica: string; readonly document: object }
  | { readonly op: "import"; readonly replica: string; readonly schemaVersion: number; readonly updateBase64: string }
  | { readonly op: "export"; readonly replica: string }
  | { readonly op: "exportIncremental"; readonly replica: string; readonly frontierBase64: string }
  | { readonly op: "frontier"; readonly replica: string }
  | { readonly op: "materialize"; readonly replica: string; readonly revision: string }
  | { readonly op: "captureText"; readonly replica: string; readonly field: string; readonly identities: Identities }
  | { readonly op: "readText"; readonly replica: string; readonly field: string; readonly identities: Identities }
  | { readonly op: "insertText"; readonly replica: string; readonly field: string; readonly identities: Identities; readonly offset: number; readonly text: string }
  | { readonly op: "policyConflicts"; readonly entity: string; readonly base: object; readonly client: object; readonly current: object; readonly kind: CollaborationWriterKind };

interface CollaborationFixtures {
  readonly entities: readonly { readonly entity: string; readonly clientDocument: object }[];
}

type Replica = CollaborationReplica<object>;

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

/** The version of the `@clerkenwell/client` package this bridge resolves. */
function clientVersion(): string {
  const entry = fileURLToPath(import.meta.resolve("@clerkenwell/client"));
  for (let directory = path.dirname(entry); ; directory = path.dirname(directory)) {
    const manifestPath = path.join(directory, "package.json");
    if (existsSync(manifestPath)) {
      const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as { readonly name?: string; readonly version?: string };
      if (manifest.name === "@clerkenwell/client" && manifest.version !== undefined) {
        return manifest.version;
      }
    }
    if (path.dirname(directory) === directory) {
      throw new Error(`${entry} is in no @clerkenwell/client package.`);
    }
  }
}

function handle(request: Request): unknown {
  switch (request.op) {
    case "clientVersion":
      return clientVersion();
    case "seedFixture": {
      const fixture = fixtures.entities.find(({ entity }) => entity === request.entity);
      if (fixture === undefined) {
        throw new Error(`The fixture corpus has no ${request.entity} entity.`);
      }
      return create(request.replica, CollaborationReplica.from(
        request.entity,
        plan(request.entity),
        { kind: "document", document: fixture.clientDocument },
        request.peerBlock
      ));
    }
    case "hydrate": {
      const entityPlan = plan(request.entity);
      requireCollaborationSchemaVersion(request.entity, entityPlan, request.schemaVersion);
      return create(request.replica, CollaborationReplica.from(
        request.entity,
        entityPlan,
        { kind: "update", updateBase64: request.updateBase64 },
        request.peerBlock
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
    case "policyConflicts":
      return conflictingFieldPaths(
        plan(request.entity),
        request.base,
        request.client,
        request.current,
        request.kind
      );
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
