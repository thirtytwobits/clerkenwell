# Changelog

Each release's section is the notes of its GitHub release. A release that changes a contract
lists each change under **Breaking**; `cargo run -p clerkenwell-release -- breaking --help`
names the contracts.

## 0.2.0

### Breaking

- The session protocol's wire types in `clerkenwell-session`, and `CollaborationExchangeMode` in
  `clerkenwell-store`, derive schemars' `JsonSchema`.
- `clerkenwell-session` and `clerkenwell-axum` record the protocol's wire form in
  `wire-protocol.json`, a contract a patch release may not change. Their `testing` features
  build the records.

### Changed

- `clerkenwell-axum` writes its JSON-RPC frames from typed values. The frames carry the same
  members and values; `jsonrpc` and `id` now come before `result` or `error`.

## 0.1.0

The first release.

- Crates, taken from this repository at the tag: `clerkenwell-schema`, `clerkenwell-doc`,
  `clerkenwell-session`, `clerkenwell-axum`, `clerkenwell-events`, `clerkenwell-store`,
  `clerkenwell-codegen` and `clerkenwell-conformance`.
- npm packages, attached to the release: `@clerkenwell/client` and `@clerkenwell/react`.
- Loro: the `loro` crate and the `loro-crdt` package from the release pair `loro-release.json`
  records.
