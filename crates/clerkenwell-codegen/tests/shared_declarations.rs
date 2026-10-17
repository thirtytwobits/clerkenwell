//! The declarations every collaborative entity's authoring state shares are
//! Clerkenwell's types: the schemas the generator declares describe them, and
//! generated bindings name them rather than declaring their own.

mod common;

use std::path::Path;

use clerkenwell_codegen::testing::Json;
use clerkenwell_codegen::{build_outputs, Config, Definition, GeneratedOutputs};
use clerkenwell_schema::{
    AuthoringPeerBlock, AuthoringState, AuthoringStatePatch, CollaborationExchangeMode,
};
use common::{expanded_notebook, json, notebook_config, notebook_document, set, validate};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

/// The declarations the generator writes for every collaborative entity's
/// authoring state.
const SHARED: &[&str] = &[
    "AuthoringState",
    "AuthoringStatePatch",
    "AuthoringPeerBlock",
    "CollaborationExchangeMode",
];
/// The types `clerkenwell_schema` declares for them, with the enums they hold.
const SHARED_RUST: &[&str] = &[
    "AuthoringPeerBlock",
    "AuthoringState",
    "AuthoringStatePatch",
    "AuthoringStatePatchKind",
    "CollaborationExchangeMode",
];

fn notes_config() -> Config {
    Config::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/notes/clerkenwell-codegen.json"),
    )
    .expect("the notes configuration loads")
}

fn outputs(definition: &Definition, config: &Config) -> GeneratedOutputs {
    build_outputs(definition, config).expect("renders")
}

/// The outputs of the definition `config` names.
fn rendered(config: &Config) -> GeneratedOutputs {
    let definition = Definition::load(&config.definition).expect("valid");
    outputs(&definition, config)
}

/// A validator of the shared declaration `name`, as the notebook's expanded
/// definition declares it.
fn shared_schema(name: &str) -> jsonschema::Validator {
    let defs = expanded_notebook()
        .pointer("/$defs")
        .and_then(Json::as_object)
        .expect("the notebook declares $defs")
        .clone();
    let mut shared = serde_json::Map::new();
    for declared in SHARED {
        let schema = defs
            .get(declared)
            .unwrap_or_else(|| panic!("the generator declares {declared}"));
        shared.insert((*declared).to_owned(), schema.to_serde());
    }
    let schema = serde_json::json!({ "$defs": shared, "$ref": format!("#/$defs/{name}") });
    jsonschema::validator_for(&schema).expect("a shared declaration is a schema")
}

fn state(schema_version: u32, index_bits: u32) -> AuthoringState {
    AuthoringState {
        schema_version,
        accepted_frontier_base64: "frontier".to_owned(),
        update_base64: "update".to_owned(),
        etag: "etag".to_owned(),
        exchange_modes: vec![
            CollaborationExchangeMode::Incremental,
            CollaborationExchangeMode::Bootstrap,
        ],
        peer_block: AuthoringPeerBlock {
            nonce: "nonce".to_owned(),
            base: u64::MAX.to_string(),
            index_bits,
        },
        replaces_held: false,
    }
}

fn states() -> Vec<AuthoringState> {
    vec![state(0, 0), state(1, 16), state(u32::MAX, u32::MAX)]
}

fn patches() -> Vec<AuthoringStatePatch> {
    states()
        .into_iter()
        .map(AuthoringStatePatch::replace)
        .chain([AuthoringStatePatch::remove()])
        .collect()
}

fn wire(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("serialises")
}

fn read<T: DeserializeOwned>(value: &Value) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}

#[test]
fn the_shared_schemas_accept_every_value_clerkenwells_types_write() {
    let states_schema = shared_schema("AuthoringState");
    for written in states() {
        let value = wire(&written);
        assert!(states_schema.is_valid(&value), "{value}");
    }
    let patches_schema = shared_schema("AuthoringStatePatch");
    for written in patches() {
        let value = wire(&written);
        assert!(patches_schema.is_valid(&value), "{value}");
    }
}

#[test]
fn clerkenwells_types_read_back_every_value_the_shared_schemas_accept() {
    let states_schema = shared_schema("AuthoringState");
    for written in states() {
        let value = wire(&written);
        assert!(states_schema.is_valid(&value));
        assert_eq!(read::<AuthoringState>(&value), Some(written));
    }
    let patches_schema = shared_schema("AuthoringStatePatch");
    for written in patches() {
        let value = wire(&written);
        assert!(patches_schema.is_valid(&value));
        assert_eq!(read::<AuthoringStatePatch>(&value), Some(written));
    }
}

#[test]
fn clerkenwells_types_refuse_what_the_shared_schemas_refuse() {
    let schema = shared_schema("AuthoringState");
    let valid = wire(&state(1, 16));
    let mut refused = Vec::new();
    for (pointer, replacement) in [
        ("/schema_version", json("-1")),
        ("/schema_version", json("4294967296")),
        ("/peer_block/index_bits", json("4294967296")),
        ("/exchange_modes", json(r#"["optimisticDocument"]"#)),
        ("/undeclared", json("true")),
        ("/peer_block/undeclared", json("true")),
    ] {
        let mut value = Json::parse(&valid.to_string()).expect("JSON");
        set(&mut value, pointer, replacement);
        refused.push(value.to_serde());
    }
    for value in refused {
        assert!(!schema.is_valid(&value), "the schema refuses {value}");
        assert!(
            read::<AuthoringState>(&value).is_none(),
            "the type refuses {value}"
        );
    }
}

/// The names `pub use clerkenwell_schema::{..};` re-exports.
fn rust_reexports(rust: &str) -> Vec<String> {
    let start = rust
        .find("pub use clerkenwell_schema::{")
        .expect("the module re-exports clerkenwell_schema");
    let list = &rust[start..];
    let list = &list[list.find('{').expect("a brace") + 1..list.find('}').expect("a brace")];
    list.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

fn rust_declares(rust: &str, name: &str) -> bool {
    rust.lines().any(|line| {
        line == format!("pub struct {name} {{")
            || line == format!("pub enum {name} {{")
            || line.starts_with(&format!("pub type {name} ="))
    })
}

/// The names of every `{..}` list the TypeScript module opens with `head`.
fn typescript_list(typescript: &str, head: &str, tail: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = typescript;
    while let Some(start) = rest.find(head) {
        let list = &rest[start + head.len()..];
        let end = list.find('}').expect("a closing brace");
        if list[end..].starts_with(tail) {
            names.extend(
                list[..end]
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
            );
        }
        rest = &list[end..];
    }
    names
}

fn typescript_declares(typescript: &str, name: &str) -> bool {
    typescript
        .lines()
        .any(|line| line.starts_with(&format!("export type {name} =")))
}

#[test]
fn generated_bindings_name_clerkenwells_types_for_the_shared_declarations() {
    for config in [notebook_config(), notes_config()] {
        let generated = rendered(&config);
        let reexported = rust_reexports(&generated.rust_model);
        let imported = typescript_list(
            &generated.typescript_model,
            "import type {",
            "} from \"@clerkenwell/client\";",
        );
        let exported = typescript_list(&generated.typescript_model, "export type {", "};");
        for name in SHARED_RUST {
            assert!(
                !rust_declares(&generated.rust_model, name),
                "{}: {name}",
                config.definition.display()
            );
            assert!(
                reexported.iter().any(|reexport| reexport == name),
                "{}: {name}",
                config.definition.display()
            );
        }
        for name in SHARED {
            assert!(
                !typescript_declares(&generated.typescript_model, name),
                "{}: {name}",
                config.definition.display()
            );
            assert!(imported.iter().any(|import| import == name), "{name}");
            assert!(exported.iter().any(|export| export == name), "{name}");
        }
    }
}

#[test]
fn a_definition_names_a_shared_declaration_as_clerkenwells_type() {
    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/ReplicaUpdateParams/properties/exchange_mode",
        json(r##"{ "$ref": "#/$defs/CollaborationExchangeMode" }"##),
    );
    let definition = validate(document).expect("valid");

    let generated = outputs(&definition, &notebook_config());

    assert!(
        generated
            .rust_model
            .contains("pub exchange_mode: CollaborationExchangeMode,"),
        "{}",
        generated.rust_model
    );
    assert!(!rust_declares(
        &generated.rust_model,
        "CollaborationExchangeMode"
    ));
    assert!(
        generated
            .typescript_model
            .contains("\"exchange_mode\": CollaborationExchangeMode;"),
        "{}",
        generated.typescript_model
    );
}
