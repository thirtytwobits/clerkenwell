# Releasing

Every crate and package is released together at one version. In 0.x, a release that changes a
contract is a minor release and any other is a patch; `cargo run -p clerkenwell-release --
breaking --help` names the contracts.

1. Move every manifest and lockfile to the new version:
   `cargo run -p clerkenwell-release -- bump X.Y.Z`.
2. Add a `## X.Y.Z` section to `CHANGELOG.md`, listing each contract change under
   `### Breaking`.
3. Compare the release with the previous one:
   `cargo run -p clerkenwell-release -- breaking --since vA.B.C`.
4. Merge the change to `main`.
5. Tag the merge commit and push the tag: `git tag vX.Y.Z <commit>`, then
   `git push origin vX.Y.Z`.

Pushing the tag runs `.github/workflows/release.yml`: the CI gate, a check that the tag names
the version every manifest states, the comparison with the previous release, and
`npm run package:check`. It then publishes the GitHub release with the version's changelog
section as its notes and both npm tarballs attached.
