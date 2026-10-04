# Notes example

A walk-through of Clerkenwell over one collaborative note: a title writers overwrite, a body
whose concurrent edits merge, and a status whose concurrent changes are resolved explicitly.

```bash
cargo run -p clerkenwell-example-notes
```

The binary runs these steps against a store in memory:

1. The store creates the note from its seed document.
2. Two writers edit the body concurrently and one retitles the note. Each commit is fenced on
   the frontier its writer read, so the body keeps both edits.
3. A writer whose replica began outside the store is refused and told to resynchronise.
4. A writer fences its edit on the etag it read. Another writer commits first, so the edit is
   refused as stale and the refusal carries the accepted note and its etag.
5. Two writers set the status differently. The second commit is refused as a conflict naming
   `status`, and the refusal carries the accepted note.
6. The refused writer rebases onto the accepted note, restates its status and commits.
7. An export and a reindex are recorded in the recovery audit.

Each step is a function in `src/lib.rs`; `tests/walkthrough.rs` checks each outcome.

## Server

```bash
cargo run -p clerkenwell-example-notes --bin notes-server
```

`notes-server` serves the notes through `clerkenwell-axum` and prints the WebSocket address.
It keeps the notes in memory through `src/storage.rs`, the example's implementation of the
storage port.
A client creates a note with `note.create`, subscribes to `notes.authoringState` for the
accepted frontier and operations, and sends its replica's operations with
`note.importUpdate`. When an edit is accepted, every subscriber to the note takes the
operations it lacks. A refused edit
carries the operations its writer lacks, which the writer takes before sending its edit
again. `src/server.rs` holds the application; `tests/server.rs` runs two clients against it.

## Files

| Path | Holds |
|---|---|
| `notes.projections.json` | the definition |
| `clerkenwell-codegen.json` | the generator configuration |
| `src/model.rs`, `generated/` | the bindings generated from the definition |
| `src/server.rs`, `src/bin/notes-server.rs` | the notes served over the projection protocol |
| `src/storage.rs` | the storage port the notes are kept through |

`tests/generated.rs` fails when the bindings are stale. Regenerate them from the repository
root with `cargo run -p clerkenwell-codegen -- --config examples/notes/clerkenwell-codegen.json`.
