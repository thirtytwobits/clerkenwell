# Clerkenwell

Clerkenwell is a library for applications in which a person and an AI agent edit the same
documents at the same time.

Say you keep notes in a small editor and ask an agent to rewrite the second paragraph of one.
If the agent writes the file while you are typing, one of you loses work. A collaborative
editor of the kind behind Google Docs solves that with user accounts, logins and a sync service
to deploy and run. Clerkenwell gives a CLI or a GUI live collaboration with one server process.

The server holds the accepted copy of each document. Your editor and the agent's tools each
keep their own copy, send their edits to the server, and receive each other's edits as the
server accepts them. [Loro](https://github.com/loro-dev/loro) merges the edits; Clerkenwell
decides what the server accepts and carries edits between the server and its clients.

Clerkenwell has no accounts or logins: anyone who can reach the server can edit. The example
server listens only on the local machine.

## Examples

**The agent rewrites a paragraph while you keep typing.** The agent reads the note, rewrites
the second paragraph and sends the result through a CLI command or MCP tool your application
provides. Meanwhile you add a sentence to the first paragraph. The server records only the
characters the agent changed, so the note keeps both edits.

**The agent changes something you have already decided.** You set a note's status to
published. The agent, working from the copy it read earlier, sets it to review. The server
refuses the agent's change, names the field and returns the note as it stands, so the agent
decides again from what you wrote. Each field has its own rule for concurrent changes: keep
both, keep the later one, refuse the second, or refuse any change once the document exists.

**The agent retries a tool call.** A tool call times out and the agent sends the same edit
again. The server recognises the edit by the id its client gave it and records it once.

## Building an application

1. Describe your documents in a JSON definition that names each field and its rule for
   concurrent changes. [`examples/notes/notes.projections.json`](examples/notes/notes.projections.json)
   describes a note.
2. Generate Rust and TypeScript bindings from the definition with `clerkenwell-codegen`.
3. Serve the documents. [`examples/notes/src/server.rs`](examples/notes/src/server.rs) serves
   notes over WebSocket.
4. Connect a GUI through `@clerkenwell/react`, or a CLI or MCP server through
   `@clerkenwell/client` under Node.

Run the walk-through, which edits one note from two writers and prints each refusal:

```bash
cargo run -p clerkenwell-example-notes
```

Serve notes to WebSocket clients:

```bash
cargo run -p clerkenwell-example-notes --bin notes-server
```

## Documentation

| Document | Covers |
|---|---|
| [`examples/notes/README.md`](examples/notes/README.md) | the walk-through step by step, and the notes server |
| [`docs/architecture.md`](docs/architecture.md) | terms, crates and TypeScript packages |
| [`docs/using-a-release.md`](docs/using-a-release.md) | depending on a released crate, package or generator |
| [`CHANGELOG.md`](CHANGELOG.md) | what each release changed |
| [`RELEASING.md`](RELEASING.md) | cutting a release |

## Licence

MIT. See [`LICENSE`](LICENSE).
