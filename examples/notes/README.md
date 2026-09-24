# Notes example

A walk-through of Clerkenwell over one collaborative note: a title writers overwrite, a body
whose concurrent edits merge, and a status whose concurrent changes are resolved explicitly.

```bash
cargo run -p clerkenwell-example-notes
```

The binary runs these steps against a store in a temporary directory:

1. The store creates the note from its seed document.
2. Two writers edit the body concurrently and one retitles the note. Each commit is fenced on
   the frontier its writer read, so the body keeps both edits.
3. A writer whose replica began outside the store is refused and told to resynchronise.
4. Two writers set the status differently. The second commit is refused as a conflict naming
   `status`, and the refusal carries the accepted note.
5. The refused writer rebases onto the accepted note, restates its status and commits.
6. An export and a reindex are recorded in the recovery audit.

Each step is a function in `src/lib.rs`; `tests/walkthrough.rs` checks each outcome.

## Files

| Path | Holds |
|---|---|
| `notes.projections.json` | the definition |
| `clerkenwell-codegen.json` | the generator configuration |
| `src/model.rs`, `generated/` | the bindings generated from the definition |

`tests/generated.rs` fails when the bindings are stale. Regenerate them from the repository
root with `cargo run -p clerkenwell-codegen -- --config examples/notes/clerkenwell-codegen.json`.
