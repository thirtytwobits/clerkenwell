mod common;
use clerkenwell_codegen::{build_outputs, Config, Definition};
use serde_json::json;
use std::fs;

#[test]
fn imports_typed_recursive_records_unions_and_open_extension_slots() {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("shared.schema.json");
    fs::write(&schema, json!({"$defs": {
        "SharedMode": {"type": "string", "enum": ["compact", "expanded"]},
        "SharedRecord": {"type": "object", "additionalProperties": false, "required": ["revision", "mode", "extensions"], "properties": {
            "revision": {"type": "integer", "minimum": 0},
            "mode": {"$ref": "#/$defs/SharedMode"},
            "parent": {"$ref": "#/$defs/SharedRecord"},
            "extensions": {"type": "object", "additionalProperties": true}
        }},
        "SharedChange": {"oneOf": [
            {"type": "object", "additionalProperties": false, "required": ["operation", "record"], "properties": {"operation": {"const": "insert"}, "record": {"$ref": "#/$defs/SharedRecord"}}},
            {"type": "object", "additionalProperties": false, "required": ["operation"], "properties": {"operation": {"const": "remove"}}}
        ]}
    }}).to_string()).unwrap();
    let mut config = common::notebook_config();
    config.schema_imports = vec![schema];
    let definition =
        Definition::load_with_schemas(&config.definition, &config.schema_imports).unwrap();
    let outputs = build_outputs(&definition, &config).unwrap();
    assert!(outputs.rust_model.contains("Option<Box<SharedRecord>>"));
    assert!(outputs.typescript_model.contains("SharedRecord"));
    assert!(outputs.rust_model.contains("pub enum SharedMode"));
    assert!(outputs.rust_model.contains("pub enum SharedChange"));
    let normalised: serde_json::Value =
        serde_json::from_str(&outputs.normalized_definition).unwrap();
    assert!(normalised.to_string().contains("minimum"));
}

#[test]
fn imports_refuse_duplicate_ownership_and_unsupported_constraints() {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("shared.schema.json");
    fs::write(&schema, json!({"$defs": {"Shared": {"type": "object", "additionalProperties": false, "properties": {}}}}).to_string()).unwrap();
    let config = common::notebook_config();
    assert!(
        Definition::load_with_schemas(&config.definition, &[schema.clone(), schema.clone()])
            .is_err()
    );
    fs::write(
        &schema,
        json!({"$defs": {"Shared": {"type": "string", "pattern": "[a-z]+"}}}).to_string(),
    )
    .unwrap();
    assert!(Definition::load_with_schemas(&config.definition, &[schema]).is_err());
}

#[test]
fn imported_paths_resolve_against_configuration_and_reads_leave_sources_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codegen.json");
    let source = json!({"definition": "definition.json", "schemaImports": ["assets/shared.schema.json"], "regenerateCommand": "generate", "outputs": {
        "collaborationFixtures": "collaboration.json", "contractFixtures": "contract.json", "normalizedDefinition": "ir.json", "typescriptModel": "model.ts", "rustModel": "model.rs"
    }}).to_string();
    fs::write(&path, &source).unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(
        config.schema_imports,
        vec![directory.path().join("assets/shared.schema.json")]
    );
    assert_eq!(fs::read_to_string(path).unwrap(), source);
}

#[test]
fn numeric_format_intersection_preserves_stricter_fractional_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("numbers.json");
    let imported = json!({"$defs": {"ImportedQuantity": {"type": "object", "additionalProperties": false, "properties": {"amount": {"type": "integer", "format": "uint32", "minimum": 0.5, "maximum": 3.5}}}}});
    fs::write(&path, imported.to_string()).unwrap();
    let mut config = common::notebook_config();
    config.schema_imports = vec![path.clone()];
    let definition =
        Definition::load_with_schemas(&config.definition, &config.schema_imports).unwrap();
    let outputs = build_outputs(&definition, &config).unwrap();
    let normalised: serde_json::Value =
        serde_json::from_str(&outputs.normalized_definition).unwrap();
    let quantity = normalised["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|definition| definition["name"] == "ImportedQuantity")
        .unwrap();
    for bound in ["minimum", "maximum"] {
        assert_eq!(
            quantity["properties"]["amount"][bound],
            imported["$defs"]["ImportedQuantity"]["properties"]["amount"][bound]
        );
    }
    for malformed in [
        json!({"type": "integer", "format": "uint32", "minimum": "zero"}),
        json!({"type": "integer", "minimum": 2, "maximum": 1}),
        json!({"type": "string", "minimum": 0}),
        json!({"$ref": "#/$defs/Missing"}),
        json!({"type": "integer", "format": "unknown"}),
    ] {
        fs::write(
            &path,
            json!({"$defs": {"ImportedQuantity": malformed}}).to_string(),
        )
        .unwrap();
        assert!(Definition::load_with_schemas(&config.definition, &config.schema_imports).is_err());
    }
}

#[test]
fn reference_annotations_preserve_defaults_and_validate_the_referenced_type() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("annotations.json");
    let mut imported = json!({"$defs": {
        "Choice": {"type": "string", "enum": ["first", "second"]},
        "Selection": {"type": "object", "additionalProperties": false, "properties": {
            "choice": {"$ref": "#/$defs/Choice", "default": "second", "description": "Selection"}
        }}
    }});
    let config = common::notebook_config();
    fs::write(&path, imported.to_string()).unwrap();
    let definition =
        Definition::load_with_schemas(&config.definition, std::slice::from_ref(&path)).unwrap();
    assert!(build_outputs(&definition, &config).is_ok());
    imported["$defs"]["Selection"]["properties"]["choice"]["default"] = json!(false);
    fs::write(&path, imported.to_string()).unwrap();
    assert!(Definition::load_with_schemas(&config.definition, &[path]).is_err());
}
