# Clerkenwell

Schema-driven collaborative documents over [Loro](https://github.com/loro-dev/loro): a
declarative document language, plans generated from it for Rust and TypeScript, replicas that
execute those plans, and a durable single-authority store with fenced commits, retained
operations and forensic recovery.

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
| `clerkenwell-conformance` | cross-language conformance: Rust and TypeScript replicas of one definition's entities exchange the generated fixture operations, concurrent edits and text consumption, and must converge; TypeScript clients sync a note through the Rust notes server |

`loro-release.json` records the approved Rust and npm Loro release pair. No public API
names a Loro type or is named after Loro; an application that uses Loro depends on it itself.

## Example

`examples/notes` walks through a store, two writers editing one note concurrently, fenced
commits, a conflict on an explicit field resolved by rebasing, and the recovery audit:
`cargo run -p clerkenwell-example-notes`. Its `notes-server` binary serves the same notes to
WebSocket clients: `cargo run -p clerkenwell-example-notes --bin notes-server`.

## TypeScript packages

An npm workspace under `clients/typescript/`, consumed as TypeScript source. `npm run typecheck`
and `npm test` run from the repository root.

| Package | Owns |
|---|---|
| `@clerkenwell/client` | plan types, projection materialisation and subscriptions, the projection protocol over a JSON-RPC WebSocket with reconnection, the authoring runtime and its persisted form, text bindings, field conflict policy |
| `@clerkenwell/client/replica` | plan-driven Loro replicas, read and written as an application's drafts; the package's only importer of `loro-crdt` |
| `@clerkenwell/react` | React stores, edit overlays, text-binding hooks, autosync and the authoring runtime provider |

## Licence

MIT. See [`LICENSE`](LICENSE).
