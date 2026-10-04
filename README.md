# Clerkenwell

Clerkenwell is a Rust and TypeScript library for applications in which people and AI agents
edit the same documents. An agent is a second writer on every document: it is fast, often
works from a stale read, retries on failure, and must not decide some things. Clerkenwell
exists to refuse that writer safely.

A server holds the authoritative copy of each document. Human and agent clients edit replicas
of it and send their edits to the server. [Loro](https://github.com/loro-dev/loro) merges the
edits. Clerkenwell is the rest: the schema that declares each document, the store that keeps
it, the rules for what the server accepts, and the protocol clients sync over.

| The agent | Clerkenwell |
|---|---|
| writes from a stale read | the etag fence refuses the write and returns the accepted document |
| races a human on a decision | a field with `explicit` conflict policy refuses the concurrent change and names the field |
| appends prose a human is also editing | a field with `merge` policy keeps both edits |
| retries a tool call | the client-minted operation id makes the replay a recorded duplicate |
| writes a shape the application does not understand | the store runs the application's validation on every document it accepts and refuses one that fails |

`examples/notes` sets a note's status from two writers at once. The status has `explicit`
conflict policy, so the second writer is refused:

```text
5. Ada set the status first; Grace's concurrent status was refused:
Conflict: Concurrent Note edits require explicit resolution for: status.
```

## Terms

| Term | Meaning |
|---|---|
| definition | the JSON file declaring an application's entities, projections, mutations and, for each collaborative field, its storage and conflict policy |
| plan | what the generator emits from a definition for each collaborative entity; replicas execute it to read and write fields |
| replica | one writer's copy of a document, which records its edits as operations |
| projection | a named view of application state that clients subscribe to |
| authoring state | the projection each collaborative entity declares: the accepted document as operations a replica takes |
| frontier | the version of a document a replica has seen, named by its latest operations |
| etag | a digest of a document's accepted state |
| fence | the etag or frontier a write was made from; the server refuses a write whose fence the accepted document has moved past |
| conflict policy | how concurrent changes to one field are judged: `merge` keeps both, `lastWriterWins` keeps the later, `explicit` refuses the second and names the field, `immutable` refuses any change |
| envelope | the stored record of one document: a checkpoint, the operations kept since it, and the document's generation |
| residency | the documents a store holds in memory, served while the storage confirms the stored bytes are unchanged |
| exchange mode | whether a write creates a document (`bootstrap`) or edits one (`incremental`) |

## Scope

Clerkenwell has no accounts, logins or permissions: anyone who can reach its server can edit.
Run the server where only the application's people and agents can reach it.

## Trying it

```bash
cargo run -p clerkenwell-example-notes
cargo run -p clerkenwell-example-notes --bin notes-server
```

The first runs the walk-through above; the second serves notes to WebSocket clients.

## Documentation

| Document | Covers |
|---|---|
| [`examples/notes/README.md`](examples/notes/README.md) | the walk-through step by step, and the notes server |
| [`docs/architecture.md`](docs/architecture.md) | crates, packages and directories |
| [`docs/using-a-release.md`](docs/using-a-release.md) | depending on a released crate, package or generator |
| [`CHANGELOG.md`](CHANGELOG.md) | what each release changed |

## Licence

MIT. See [`LICENSE`](LICENSE).
