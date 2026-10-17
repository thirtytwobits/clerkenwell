# Changelog

Each release's section is the notes of its GitHub release. A release that changes a contract
lists each change under **Breaking**; `cargo run -p clerkenwell-release -- breaking --help`
names the contracts.

## 0.7.0

### Breaking

- A TypeScript replica reads and writes a collaborative document as the application's schema
  spells it, at every depth and in both directions (#77). `@clerkenwell/client` respelled each
  member name a field path declares, and each key of a structured list's entries, from
  snake_case to camelCase when it materialised a document, and back when it wrote one. The
  documents a replica materialises, takes, validates and judges against conflict policy, and
  the paths its errors name, are in the schema's spelling.
- The generated TypeScript module declares `CollaborationDocument<TEntity>` in place of
  `Client<Name>` and `CollaborationClientDocument<TEntity>` (#74). It names each collaborative
  entity's document as a replica holds it: the entity's generated document type, through
  `CollaborationReplicaDocument<TDocument, TPath>`, which `@clerkenwell/client` exports and
  which makes the member at each dotted path of `TPath` optional. The paths are those at which
  a replica may leave out a member the schema requires: a `derivedIdentity` member outside
  keyed items, every `derivedRevision` member, and an object member no field beneath which is
  in every document.
- `clerkenwell-codegen` drops its refusals of an ambiguous client spelling, and refuses a
  `$defs` entry named `CollaborationDocument`. The collaboration fixtures drop
  `clientDocument`; both replicas take `wireDocument`.
- `clerkenwell-conformance` removes `check_client_naming`. `Conformance::client_document_types`
  is `Conformance::document_types`, which checks each document TypeScript materialises against
  its entity's `CollaborationDocument`. `Conformance::seeding` and
  `Conformance::fixture_operations` require each replica to materialise the fixture document,
  and the document after each fixture edit, as the wire document a replica holds of it.

### Fixed

- A structured-list entry key spelled with an upper-case letter, such as `fileName`, is written
  back as spelled. `@clerkenwell/client` wrote it back as `file_name`.

## 0.6.0

### Breaking

- `clerkenwell-schema` owns the authoring state's wire types: `AuthoringState`,
  `AuthoringStatePatch`, `AuthoringStatePatchKind`, `AuthoringPeerBlock` and
  `CollaborationExchangeMode` (#73). It depends on serde and schemars. Generated Rust
  re-exports these types, where it declared `AuthoringState`, `AuthoringStatePatch`,
  `AuthoringPeerBlock`, `AuthoringStateExchangeModes` and `AuthoringStatePatchKind` of its own:
  `schema_version` and `index_bits` are `u32`, and `exchange_modes` holds
  `CollaborationExchangeMode` values.
- `clerkenwell_session::AuthoringState` and `clerkenwell_session::AuthoringPeerBlock` are
  removed; the session delivers `clerkenwell_schema`'s types. Its wire record adds the
  authoring-state patch, and the authoring state and its peer block refuse members they do not
  declare. A patch's `state` is absent or an authoring state, never `null`.
- `clerkenwell_store::CollaborationExchangeMode` is removed; `CollaborationImportRequest` takes
  `clerkenwell_schema`'s. `CollaborationAuthoringState::into_authoring_state` gives a read as an
  authoring state delivers it to the writer of a `PeerBlock`.
- The generator declares `CollaborationExchangeMode` beside the authoring state, and a
  definition may reference it but not declare it. The authoring state's `schema_version` and
  `index_bits` carry the bounds of `u32`.
- `@clerkenwell/client` exports `AuthoringState`, `AuthoringStatePatch`, `AuthoringPeerBlock` and
  `CollaborationExchangeMode`, and generated TypeScript re-exports them where it declared its
  own. Replicas take an `AuthoringPeerBlock`, and `@clerkenwell/client/replica`'s
  `CollaborationPeerBlock` is removed. `AuthoringExchangeMode` is a `CollaborationExchangeMode`
  or `optimisticDocument`.
- `clerkenwell-codegen` refuses a definition whose client spelling is ambiguous: an object two
  of whose members client documents spell alike, or a member one field holds in wire spelling
  while another respells it differently. It refuses a `$defs` entry named as a client document
  type.
- `clerkenwell-conformance` adds `Conformance::client_document_types`: TypeScript materialises
  each fixture document, and the document after each fixture edit, and the TypeScript compiler
  checks each against its entity's client document type. It needs `typescript` installed in the
  project it runs in.

### Added

- The generated TypeScript module declares `Client<Name>` for each `$defs` entry a
  collaborative document holds in client spelling, and `CollaborationClientDocument<TEntity>`
  for each collaborative entity's document (#74). A member stored whole as a structured map, a
  structured document or property text has its wire type. A `derivedIdentity` member at the
  document root, and every `derivedRevision` member, is optional: a replica does not always
  materialise them. So is an object member no field beneath which is in every document, since a
  replica writes an object only beneath a field it writes.

### Fixed

- `@clerkenwell/client` respells the keys of a structured list's entries at every depth. An
  object in an array nested in an array kept its wire spelling.
- The collaboration fixtures' `clientDocument` keeps the wire spelling of structured maps,
  structured documents and property text, as a replica does, and `clerkenwell-conformance`
  respells a structured list's entries at every depth.
- The collaboration fixtures keep the enum values and union tags inside structured values, and
  the fixture scalar edit writes a value of its field's type.

## 0.5.0

### Breaking

- `@clerkenwell/client` exports `Timers` and `PLATFORM_TIMERS`. `RpcSocket`,
  `subscribeProjection` and `watchProjection` take `timers`, the `Timers` they wait by: when a
  socket reopens, and when a wait for a first snapshot gives up. `PLATFORM_TIMERS`, the
  platform's timers and clock, is the default.

## 0.4.1

### Fixed

- Generated Rust schema bounds use a numeric suffix when an integer exceeds `i32`.
  Unsigned 32-bit contracts compile with their full range.

## 0.4.0

### Breaking

- `clerkenwell-codegen::Config` adds `schema_imports`. `Definition::load_with_schemas`
  composes JSON Schema bundles from explicit paths before validating the definition.

### Added

- Generator configuration accepts `schemaImports`, resolved beside the configuration file.
  Imported definitions have one owner; duplicate names and unsupported constraints are refused.
- Named string enums, reference annotations, numeric bounds and tagged object unions from
  JSON Schema are represented in the generated contract.

## 0.3.1

### Fixed

- `clerkenwell-codegen` emits the Rust enum of a string-enum property, or of an array of one,
  inside a tagged-union variant (#64). It named the enum and never declared it, so the generated
  crate did not compile. A variant's enum is named after its union, its tag and its property, so
  two variants' properties of one name stay distinct.

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
- Every change names its principal. `clerkenwell-events` adds `Principal`; a `ChangeEvent`'s
  `actorid` and `actorkind` are required, and `ChangeEvent::new` takes the actor. A
  `CollaborationImportRequest` carries its `actor`, its `peer_nonces` and an optional `intent`;
  `CollaborationService::delete` and every recovery request take the principal that made it.
  The store's own writes are made by a `System` principal named after the store.
- A writer writes a document's operations under a block of peers the store set allocates it.
  `CollaborationStores::new` and `CollaborationService::new` take a `PeerKey`, and
  `CollaborationService::allocate_peers` allocates a `PeerBlock`. An import holding operations
  under a peer no nonce of its request names for its actor is refused as
  `collaboration_peer_not_allocated`, and a commit adding operations under a peer bound to
  another principal as `collaboration_peer_bound`. `CollaborationReplica::from_document` and the
  text preparations take the peer they write under.
- Envelope format 4 records the principal and intent of each retained operation and the
  principal each peer is bound to. An envelope whose checkpoint holds operations of an unbound
  peer is corrupt. `CollaborationService::attribution` reads who made a document's operations.
- One audit stream replaces the recovery audit. `CollaborationAuditRecord` names its actor and
  holds a recovery request or a refused import; the storage port's `append_audit`, `audit` and
  `discard_audit_before` replace `append_recovery_audit` and `recovery_audit`, and
  `CollaborationService::audit` answers a `CollaborationAuditQuery`.
- A collaborative field may judge a kind of writer by another policy than its own: the
  definition's `writers` maps a kind to a policy, `GeneratedCollaborationFieldSpec::writers`
  carries it, and creating a document judges the creator's kind. `conflicting_field_paths` and
  the replica's policy checks take the writer's kind; `creation_conflicting_field_paths` judges a
  creation.
- `ProjectionSubscriptions::new` takes the connection's principal, `ProjectionHost::mutate` is
  given it, and every `AuthoringState` carries a `peer_block`. `ProjectionServer::router` takes
  the function that authenticates each connection, and refuses one it names no principal for.
- `@clerkenwell/client`: `CollaborationReplica.from` and `CollaborationDrafts`' constructors take
  a `CollaborationPeerBlock`; forks, text bindings and views write under its later peers, and
  `peerNonces()` names the blocks an import carries. A `CollaborationTextView` takes its view's
  `peer`. Persisted draft operations record `peer_nonces`. `conflictingFieldPaths` takes the
  writer's kind, and `creationConflictingFieldPaths` judges a creation.
- An import never adds a root to a document's history. `CollaborationReplica` refuses an update
  holding a root it lacks with `CollaborationReplicaError::UnrelatedHistory`, and the store refuses
  such an import with its resynchronisation refusal. That refusal carries `code: "conflict"` and
  `conflict_kind: "collaboration_resync_required"`, so it reaches a client through the session
  protocol.
- An authoring state carries `replaces_held`: the store answered a frontier that is not of the
  document's history with every accepted operation, which replace what the client holds.
- `@clerkenwell/client`: a `CollaborationReplica` refuses an update holding a history built apart
  from its own with `CollaborationUnrelatedHistoryError`. `AuthoringRuntime.replaceHistory`
  replaces a session's replica with one of the store's history, and a rejection whose category
  is `collaboration_resync_required` holds the session as `resyncRequired` until it does.

### Fixed

- A client whose document was re-seeded no longer doubles every text field or sends its earlier
  history to the store (#49). The store refuses that history, tells the client its state
  replaces what it holds, and the client replaces its replica: it takes the store's document,
  keeps unsent edits made from that document, and holds any others for recovery.
- `AuthoringRuntime.ensureController` no longer drops a session's pending work when the controller
  it attaches holds a draft or records operations but takes no documents, and its replica cannot
  take the work: the replica lacks the history the recorded operations extend, or there are none.
  The session is held as `recoveryRequired` with its draft and recorded operations as they were,
  so a replica holding that history restores them; discarding releases it.
- `disconnectAuthoringSession` leaves a blocked session blocked. It made one readable, which
  dropped it from persistence when nothing was queued and unblocked it on reconnect.
- `Cargo.lock` resolves `yoke-derive` 0.8.4. Crates.io yanked 0.8.3, the version 0.2.0 resolves,
  and `cargo install --locked` warned of it on every install of the generator (#33).

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
