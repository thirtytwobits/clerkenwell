# Using a release

Each release is a `vX.Y.Z` tag and a GitHub release; [`CHANGELOG.md`](../CHANGELOG.md)
describes each one.

| Need | How |
|---|---|
| A crate | `clerkenwell-store = { git = "https://github.com/thirtytwobits/clerkenwell", tag = "vX.Y.Z" }`, and in the workspace's root `Cargo.toml` the `[patch.crates-io]` override for `generic-btree` that this repository's `Cargo.toml` carries |
| An npm package | the tarball attached to the release: `npm install https://github.com/thirtytwobits/clerkenwell/releases/download/vX.Y.Z/clerkenwell-client-X.Y.Z.tgz`; for the React binding, `clerkenwell-react-X.Y.Z.tgz` in the same install |
| The generator | `cargo install clerkenwell-codegen --git https://github.com/thirtytwobits/clerkenwell --tag vX.Y.Z --locked` |
| Conformance | `clerkenwell-conformance` as a crate, with `tsx` and the client installed in the npm project it runs in |
| A checkout instead of a release | a Cargo `[patch]` with a path, and `npm link` to a package built with `npm run build` |

`@clerkenwell/client` installs `loro-crdt` from a GitHub release tarball.
