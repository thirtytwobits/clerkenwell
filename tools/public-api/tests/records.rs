//! Every recorded crate's public API matches its record, names no type from a
//! forbidden crate, and names nothing after Loro.

use clerkenwell_public_api::{
    record, workspace, COMMAND, FORBIDDEN, GENERATED, RECORDED, RECORD_FILE,
};

#[test]
fn every_recorded_crate_matches_its_record() {
    let stale = RECORDED
        .iter()
        .filter(|name| {
            let dir = workspace().join("crates").join(name);
            let recorded = std::fs::read_to_string(dir.join(RECORD_FILE)).unwrap_or_default();
            record(&dir).expect("the crate is readable").text() != recorded
        })
        .collect::<Vec<_>>();

    assert!(
        stale.is_empty(),
        "Public API records are stale. Run `{COMMAND}` to update: {stale:?}"
    );
}

#[test]
fn no_public_api_names_a_type_from_a_forbidden_crate() {
    for name in RECORDED {
        let record = record(&workspace().join("crates").join(name)).expect("readable");
        for forbidden in FORBIDDEN {
            assert!(
                !record.names.contains(*forbidden),
                "{name}'s public API names types from {forbidden}"
            );
        }
    }
}

#[test]
fn no_public_item_is_named_after_loro() {
    for name in RECORDED {
        let record = record(&workspace().join("crates").join(name)).expect("readable");
        let named = record
            .lines
            .iter()
            .filter(|line| line.to_lowercase().contains("loro"))
            .collect::<Vec<_>>();
        assert!(named.is_empty(), "{name}: {named:#?}");
    }
}

#[test]
fn every_crate_is_recorded_or_generated() {
    let crates = std::fs::read_dir(workspace().join("crates")).expect("the crates directory");
    for entry in crates {
        let name = entry.expect("an entry").file_name();
        let name = name.to_string_lossy();
        assert!(
            RECORDED.contains(&name.as_ref()) || GENERATED.contains(&name.as_ref()),
            "{name} is neither recorded nor generated"
        );
    }
}
