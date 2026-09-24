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
| `clerkenwell-session` | the projection session protocol: wire contracts, registry, per-connection subscriptions, retained patches and resume |
| `clerkenwell-store` | durable single-authority storage: envelopes, a replaceable storage port, fenced compare-and-swap commits, two-phase publication, quarantine, repair and an audit log |
| `clerkenwell-codegen` | the definition language, its validation, and the Rust, TypeScript and fixture generator |
| `clerkenwell-conformance` | cross-language conformance: Rust and TypeScript replicas of one definition's entities exchange the generated fixture operations, concurrent edits and text consumption, and must converge |

`loro-release.json` records the approved Rust and npm Loro release pair. Consumers take
`loro` through `clerkenwell-doc`.

## Example

`examples/notes` walks through a store, two writers editing one note concurrently, fenced
commits, a conflict on an explicit field resolved by rebasing, and the recovery audit:
`cargo run -p clerkenwell-example-notes`.

## TypeScript packages

An npm workspace under `clients/typescript/`, consumed as TypeScript source. `npm run typecheck`
and `npm test` run from the repository root.

| Package | Owns |
|---|---|
| `@clerkenwell/client` | plan types, projection materialisation and subscriptions, the authoring state machine and its persisted form, text bindings |
| `@clerkenwell/client/loro` | plan-driven Loro replicas; the package's only importer of `loro-crdt` |
| `@clerkenwell/react` | React stores, edit overlays, text-binding hooks, autosync and the authoring runtime provider |

## Licence

MIT. See [`LICENSE`](LICENSE).
