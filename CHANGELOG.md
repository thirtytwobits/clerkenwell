# Changelog

Each release's section is the notes of its GitHub release. A release that changes a contract
lists each change under **Breaking**; `cargo run -p clerkenwell-release -- breaking --help`
names the contracts.

## 0.3.0

### Breaking

- `clerkenwell-store` reads and writes envelope format `ENVELOPE_VERSION` alone and refuses an
  envelope in any other format as corrupt. `CollaborationService::upgrade_envelope`,
  `upgrade_envelope_bytes`, `RetiredEnvelopeFields` and
  `CollaborationDocumentSummary::envelope_version` are removed.
- `clerkenwell-store` keeps envelopes only through the `CollaborationStoragePort` the application
  implements, and depends on no file system. `LocalFileCollaborationStorage` is removed, and
  `CollaborationService::new` takes the port and a change source in place of a directory; it
  replaces `CollaborationService::with_storage`.

### Fixed

- `AuthoringRuntime.ensureController` no longer drops a collaborative session's pending work when
  the replica it attaches lacks the history the recorded operations extend and its controller
  takes no documents. The session is held as `recoveryRequired` with its draft and recorded
  operations as they were, so a replica holding that history restores them; discarding releases
  it.

## 0.2.1

### Fixed

- `Cargo.lock` resolves `yoke-derive` 0.8.4. Crates.io yanked 0.8.3, the version 0.2.0 resolves,
  and `cargo install --locked` warned of it on every install of the generator.

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
