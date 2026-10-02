//! Every Clerkenwell crate and package, and both lockfiles, state one version,
//! and the changelog has notes for it.

use clerkenwell_release::{agreed, release_notes, workspace, CHECK_COMMAND};

#[test]
fn every_manifest_and_lockfile_states_one_version() {
    if let Err(error) = agreed(&workspace()) {
        panic!("{error}\n`{CHECK_COMMAND}` lists the same.");
    }
}

#[test]
fn the_changelog_has_notes_for_the_version() {
    let version = agreed(&workspace()).expect("one version");
    if let Err(error) = release_notes(&workspace(), version) {
        panic!("{error}");
    }
}
