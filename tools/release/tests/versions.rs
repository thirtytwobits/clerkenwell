//! Every Clerkenwell crate and package, and both lockfiles, state one version.

use clerkenwell_release::{agreed, workspace, CHECK_COMMAND};

#[test]
fn every_manifest_and_lockfile_states_one_version() {
    if let Err(error) = agreed(&workspace()) {
        panic!("{error}\n`{CHECK_COMMAND}` lists the same.");
    }
}
