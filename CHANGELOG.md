# Changelog

Each release's section is the notes of its GitHub release. A release that changes a contract
lists each change under **Breaking**; `cargo run -p clerkenwell-release -- breaking --help`
names the contracts.

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
