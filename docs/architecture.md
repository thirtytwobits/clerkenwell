# Architecture

## Crates

| Crate | Owns |
|---|---|
| `clerkenwell-schema` | the plan types generated bindings instantiate |
| `clerkenwell-doc` | plan-driven Loro replicas: whole-document edits written as operations under a given peer, text edits at a captured frontier, forks, the peers an update holds operations of, and field conflict policy judged by the writer's kind |
| `clerkenwell-session` | the projection session protocol: wire contracts, registry, per-connection subscriptions, the principal each connection is authenticated as, and what each update left its client holding, resuming a subscription from that, and serving each command against an application's host as that principal; it answers every collaborative entity's authoring state from the store keeping the document, with a block of peers allocated to the subscriber, and delivers each change a store announces to the subscriptions following it |
| `clerkenwell-axum` | a reference transport: the session protocol as JSON-RPC 2.0 over axum WebSocket connections, each authenticated as a principal by the application, each mutation published to every connection's subscriptions and each store change to its authoring-state subscriptions |
| `clerkenwell-events` | principals, and the change-event envelope: a CloudEvents 1.0 event naming a document, its generation, the frontiers a change took it between and the principal that made it, and the in-process feed that carries it; it depends on nothing collaborative |
| `clerkenwell-store` | durable single-authority storage: a set of named stores over whatever storage ports the application registers, accepted documents held in memory and written through, envelopes, a replaceable storage port, fenced compare-and-swap commits, the peer blocks writers write under and the principal each peer is bound to, quarantine, repair, and an audit of every recovery request and refused import; each commit, repair and deletion is announced on the store's change feed |
| `clerkenwell-codegen` | the definition language, the authoring-state projection it declares for each collaborative entity, its validation, and the Rust, TypeScript and fixture generator |
| `clerkenwell-notebook` | the example definition the tests share, and its generated bindings |
| `clerkenwell-conformance` | cross-language conformance: Rust and TypeScript replicas of one definition's entities exchange the generated fixture operations, concurrent edits and text consumption, and must converge; TypeScript clients sync a note through the Rust notes server. An application runs it in its npm project, against the `@clerkenwell/client` installed there |

## Other directories

| Directory | Holds |
|---|---|
| `clients/typescript/` | the TypeScript packages below, and `loro-package.mjs`, which builds the pinned `loro-crdt` |
| `conformance/` | the Node scripts Clerkenwell's own conformance tests run |
| `examples/notes` | a runnable walk-through of the framework over one note, and a server of notes |
| `tools/public-api` | each crate's public API record |
| `tools/release` | the one version every crate and package is released at, and the contracts a patch release keeps |
| `docs/` | this page, and [how to depend on a release](using-a-release.md) |

`loro-release.json` records the approved Rust and npm Loro release pair. No public API
names a Loro type or is named after Loro; an application that uses Loro depends on it itself.

## TypeScript packages

An npm workspace under `clients/typescript/`. Each package publishes ES modules and their
declarations, built into `dist/` under `NodeNext` resolution, so relative imports name their
`.js` file. Clerkenwell's own typecheck, tests and conformance bridge resolve the packages to
their TypeScript source through the `@clerkenwell/source` export condition; a consumer never
sets it. `npm run typecheck`, `npm test` and `npm run package:check` run from the repository
root.

| Package | Owns |
|---|---|
| `@clerkenwell/client` | plan types, projection materialisation and subscriptions, the projection protocol over a JSON-RPC WebSocket with reconnection, the authoring runtime and its persisted form, text bindings, field conflict policy |
| `@clerkenwell/client/replica` | plan-driven Loro replicas, read and written as an application's drafts; the package's only importer of `loro-crdt` |
| `@clerkenwell/react` | React stores, edit overlays, text-binding hooks, autosync and the authoring runtime provider |

The code generator's optional `schemaImports` configuration lists JSON Schema bundles with
`$defs`, resolved relative to the configuration file. Each definition name has one owner.
Supported imports include closed objects, typed maps, arrays, named string enums, recursive
references, tagged object unions, numeric bounds and opaque JSON extension fields. Unsupported
constraints fail generation. The projection definition supplies collaboration and transport
semantics for these imported types.
