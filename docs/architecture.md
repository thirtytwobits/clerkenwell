# Architecture

## Terms

| Term | Meaning |
|---|---|
| definition | the JSON file declaring an application's entities, projections, mutations and, for each collaborative field, its storage and conflict policy |
| plan | what the generator emits from a definition for each collaborative entity; replicas execute it to read and write fields |
| replica | one writer's copy of a document, which records its edits as operations |
| projection | a named view of application state that clients subscribe to |
| mutation | a named command a client sends the server |
| authoring state | the projection each collaborative entity declares: the accepted document as operations a replica takes |
| frontier | the version of a document a replica has seen, named by its latest operations |
| etag | a digest of a document's accepted state |
| fence | the etag or frontier a commit was made from; the store refuses a commit its fence does not admit |
| conflict policy | how concurrent changes to one field are judged: `merge` keeps both, `lastWriterWins` keeps the later, `explicit` refuses the second and names the field, `immutable` refuses any change |
| envelope | the stored record of one document: a checkpoint, the operations kept since it, and the document's generation |
| exchange mode | whether a commit creates a document (`bootstrap`) or edits one (`incremental`) |

## Crates

| Crate | Owns |
|---|---|
| `clerkenwell-schema` | the plan types generated bindings instantiate |
| `clerkenwell-doc` | plan-driven Loro replicas: whole-document edits written as operations, text edits at a captured frontier, forks, and field conflict policy |
| `clerkenwell-session` | the projection session protocol: wire contracts, registry, per-connection subscriptions and what each update left its client holding, resuming a subscription from that, and serving each command against an application's host; it answers every collaborative entity's authoring state from the store keeping the document, and delivers each change a store announces to the subscriptions following it |
| `clerkenwell-axum` | a reference transport: the session protocol as JSON-RPC 2.0 over axum WebSocket connections, each mutation published to every connection's subscriptions and each store change to its authoring-state subscriptions |
| `clerkenwell-events` | the change-event envelope, a CloudEvents 1.0 event naming a document, its generation and the frontiers a change took it between, and the in-process feed that carries it; it depends on nothing collaborative |
| `clerkenwell-store` | durable single-authority storage: a set of named stores over whatever storage ports the application registers, accepted documents held in memory and written through, envelopes, a replaceable storage port, fenced compare-and-swap commits, quarantine, repair and an audit log; each commit, repair and deletion is announced on the store's change feed |
| `clerkenwell-codegen` | the definition language, the authoring-state projection it declares for each collaborative entity, its validation, and the Rust, TypeScript and fixture generator |
| `clerkenwell-conformance` | cross-language conformance: Rust and TypeScript replicas of one definition's entities exchange the generated fixture operations, concurrent edits and text consumption, and must converge; TypeScript clients sync a note through the Rust notes server. An application runs it in its npm project, against the `@clerkenwell/client` installed there |

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
