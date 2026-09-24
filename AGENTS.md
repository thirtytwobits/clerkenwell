# Clerkenwell Agent Instructions

Every section is a hard constraint.

## Layout

```
crates/clerkenwell-schema       plan types generated bindings instantiate
crates/clerkenwell-doc          plan-driven Loro replicas, text at a frontier, conflict policy; re-exports loro
crates/clerkenwell-session      projection wire contracts, registry, subscriptions, retained patches, resume
crates/clerkenwell-store        durable envelopes, storage port, fenced commits, publication, recovery
crates/clerkenwell-codegen      the definition language, its validation, and the generator
crates/clerkenwell-notebook     the example definition the tests share, and its generated bindings
crates/clerkenwell-conformance  Rust and TypeScript replicas of one definition driven against each other
conformance/                    the Node bridges the conformance tests drive
examples/notes                  a runnable walk-through of the framework over one note
clients/typescript/             @clerkenwell/client (React-free) and @clerkenwell/react
```

## The framework knows no application

Nothing under `crates/` or `clients/` names an application's entities, projections,
mutations, paths or commands. Anything application-specific is a parameter, a plan, or
configuration. Tests use neutral fixtures (notes, tasks, boards).

## Rust builds with Cargo alone

`cargo build` never needs Node, and `cargo test` needs it only for `clerkenwell-conformance`,
whose tests drive TypeScript replicas by definition. Node is needed only for `clients/` and
those tests.

## The Loro version pair

`loro-release.json` records the approved Rust `loro` and npm `loro-crdt` pair and their
source. `Cargo.toml` pins `loro` to it and the TypeScript client depends on its npm package.
Consumers take `loro` through `clerkenwell-doc`'s re-export. Every Cargo workspace that
builds these crates carries the recorded `generic-btree` override, because workspace
patches do not propagate. Change the record, the manifests and the lockfiles together.

## Invariants

- Every import is fenced: on the etag it was read at, or on its base frontier.
- Collaborative content is written as the operations between two versions, never by
  replacing a container's whole value.
- Reads are pure: a query never writes, bootstraps, repairs or migrates.
- Corruption is reported, never materialised as an empty document.
- Generated files are never edited by hand; `clerkenwell-codegen --check` gates them.

## Validation

| Goal | Command |
|---|---|
| Format | `cargo fmt --all --check` |
| Rust tests | `npm ci`, then `cargo test --workspace` |
| Rust tests without Node | `cargo test --workspace --exclude clerkenwell-conformance` |
| TypeScript clients | `npm run typecheck && npm test` |

Run one Cargo command at a time; parallel runs contend for the same locks.

## Testing

- New behaviour adds or extends a test. The only exception is a mechanical move.
- Tests state behaviour, contracts and constraints. Do not assert incidental values: set a
  value and read it back, or check it satisfies the contract.
- Tests encode intended behaviour. Never read the implementation to decide what a test
  expects; if the implementation fails a correct test, the implementation is wrong.

## Changes

- Fix defects at the source. No guards, shims, fallbacks or parallel paths around them.
- Prefer one clear path; delete dead code and obsolete adapters when touching an area.
- Documentation is precise and concise. When something is removed, remove it cleanly:
  no note that it used to exist.
