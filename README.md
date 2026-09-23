# Clerkenwell

Schema-driven collaborative documents over [Loro](https://github.com/loro-dev/loro): a
declarative document language, plans generated from it for Rust and TypeScript, replicas that
execute those plans, and a durable single-authority store with fenced commits, retained
operations and forensic recovery.

## Crates

| Crate | Owns |
|---|---|
| `clerkenwell-codegen` | the definition language, its validation, and the Rust, TypeScript and fixture generator |

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
