//! The committed bindings are what the generator makes of the definition.

use std::path::Path;

use clerkenwell_codegen::{generate, Config, Mode};

#[test]
fn the_committed_bindings_pass_check() {
    let config =
        Config::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("clerkenwell-codegen.json"))
            .expect("the example's generator configuration loads");
    let stale = generate(&config, Mode::Check).expect("the committed bindings are current");
    assert!(stale.is_empty(), "{stale:?}");
}
