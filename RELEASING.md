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
4. Check the lockfile against the registry and RustSec: `cargo deny check advisories`.
5. Merge the change to `main`.
6. Tag the merge commit and push the tag: `git tag vX.Y.Z <commit>`, then
   `git push origin vX.Y.Z`.

A release's lockfile is what `cargo install --locked` takes from its tag, so it may hold no
yanked crate. `deny.toml` refuses yanked crates and vulnerabilities anywhere in the graph, and
unmaintained crates the workspace depends on directly. `.github/workflows/dependencies.yml`
runs the check daily and on every change to a manifest or the lockfile.

Pushing the tag runs `.github/workflows/release.yml`: the CI gate, the dependency check, a
check that the tag names the version every manifest states, the comparison with the previous
release, and `npm run package:check`. It then publishes the GitHub release with the version's changelog
section as its notes and both npm tarballs attached, and publishes the documentation site to
GitHub Pages in place of the previous release's: the README and the documents it links, and
each crate's API documentation.

<!-- newsletter:begin releasing · maintained in thirtytwobits/london -->

## Rules

- `CHANGELOG.md` holds the next release under `## Unreleased` and each release under
  `## VERSION`, with no date. An entry's subsections are `### Breaking`, `### Added`, `### Fixed`
  and `### Documentation`, in that order, each present only when it has text. A pull request that
  changes what a consumer sees adds its line under `## Unreleased`.
- A release is an annotated `vVERSION` tag on `main`. Never move or delete a pushed tag; rerun its
  workflow instead.
- The push to `main` builds the release files once. The tag workflow and publication build
  nothing.
- `tools/release.py` and `.github/workflows/release.yml` are sent from thirtytwobits/london;
  change them there.

## Procedure

`python3 tools/release.py --help` describes each step. `release.json` lists the steps
Clerkenwell adds to `prepare`, `gates` and `verify`, and the release files its main
build keeps.

1. On a release branch, run `tools/release.py prepare major|minor|patch`. It renames the
   `## Unreleased` changelog entry `## VERSION`, sets the version and runs the prepare steps.
2. Review the diff, open a pull request and run `tools/release.py gates`. Merge once CI passes.
3. The push to `main` runs CI, which keeps the release files as its `release-files` artifact for
   90 days.
4. On `main`, run `tools/release.py tag`. It requires CI to have passed on `main` for the commit,
   then pushes the annotated tag `vVERSION`.
5. The Draft release workflow creates a **draft** release with the version's changelog entry as
   its notes, and attaches that main build's files and their `SHA256SUMS`. It builds nothing. If
   it fails, rerun it for the existing tag; never move a pushed tag. If the main build's files have
   expired, rerun that build, then the Draft release workflow.
6. Review the draft's tag, commit, notes and files in GitHub, then publish it by hand.
7. Run `tools/release.py verify --issue NUMBER` to record the evidence on the release issue, then
   close the issue.

CI runs on Rust 1.99.0; the libraries support Rust 1.88 and later.

<!-- newsletter:end releasing -->
