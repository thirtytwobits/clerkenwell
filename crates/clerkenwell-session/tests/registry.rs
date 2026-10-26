use clerkenwell_schema::{
    GeneratedCommandSpec, GeneratedMaterializationPlan, GeneratedMutationSpec,
    GeneratedProjectionSpec,
};
use clerkenwell_session::ProjectionRegistry;

/// The namespace of the definition the specs here belong to.
const NAMESPACE: &str = "notebook";

/// The schema of every parameter, result and snapshot here.
const OBJECT_SCHEMA: &str = r#"{"type":"object"}"#;

/// The description of every projection, mutation and command here.
const DESCRIPTION: &str = "An operation the tests make.";

static PROJECTIONS: &[GeneratedProjectionSpec] = &[
    GeneratedProjectionSpec {
        name: "notes.list",
        description: DESCRIPTION,
        depends_on: &["Note"],
        materialization: GeneratedMaterializationPlan::Reset,
        params_schema: OBJECT_SCHEMA,
        snapshot_schema: OBJECT_SCHEMA,
    },
    GeneratedProjectionSpec {
        name: "boards.list",
        description: DESCRIPTION,
        depends_on: &["Board"],
        materialization: GeneratedMaterializationPlan::Reset,
        params_schema: OBJECT_SCHEMA,
        snapshot_schema: OBJECT_SCHEMA,
    },
];
static MUTATIONS: &[GeneratedMutationSpec] = &[
    GeneratedMutationSpec {
        name: "note.delete",
        description: DESCRIPTION,
        touches: &["Note"],
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
    },
    GeneratedMutationSpec {
        name: "board.pin",
        description: DESCRIPTION,
        touches: &["Board", "Note"],
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
    },
];
static COMMANDS: &[GeneratedCommandSpec] = &[
    GeneratedCommandSpec {
        name: "note.archiveAll",
        description: DESCRIPTION,
        touches: &["Note"],
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
    },
    GeneratedCommandSpec {
        name: "board.preview",
        description: DESCRIPTION,
        touches: &[],
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
    },
];
static REGISTRY: ProjectionRegistry =
    ProjectionRegistry::new(NAMESPACE, PROJECTIONS, MUTATIONS, COMMANDS, &[]);

#[test]
fn the_registry_names_its_definitions_namespace() {
    assert_eq!(REGISTRY.namespace(), NAMESPACE);
}

#[test]
fn every_registered_name_resolves_to_its_spec() {
    for projection in PROJECTIONS {
        assert_eq!(REGISTRY.projection(projection.name), Some(projection));
    }
    for mutation in MUTATIONS {
        assert_eq!(REGISTRY.mutation(mutation.name), Some(mutation));
    }
    for command in COMMANDS {
        assert_eq!(REGISTRY.command(command.name), Some(command));
    }
    assert_eq!(REGISTRY.projection_names().len(), PROJECTIONS.len());
    assert_eq!(REGISTRY.mutation_names().len(), MUTATIONS.len());
    assert_eq!(REGISTRY.command_names().len(), COMMANDS.len());
}

#[test]
fn arbitrary_names_never_resolve() {
    let mut state = 0x7a31_d45f_8821_0bcdu64;
    for _ in 0..10_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let candidate = format!("unknown.{state:016x}");
        assert!(REGISTRY.projection(&candidate).is_none());
        assert!(REGISTRY.mutation(&candidate).is_none());
        assert!(REGISTRY.command(&candidate).is_none());
        assert!(!REGISTRY.mutation_affects_projection(&candidate, "notes.list"));
        assert!(!REGISTRY.mutation_affects_projection("note.delete", &candidate));
        assert!(!REGISTRY.command_affects_projection(&candidate, "notes.list"));
        assert!(!REGISTRY.command_affects_projection("note.archiveAll", &candidate));
    }
}

#[test]
fn a_mutation_affects_exactly_the_projections_that_read_an_entity_it_writes() {
    for mutation in MUTATIONS {
        for projection in PROJECTIONS {
            let shares_an_entity = projection
                .depends_on
                .iter()
                .any(|entity| mutation.touches.contains(entity));
            assert_eq!(
                REGISTRY.mutation_affects_projection(mutation.name, projection.name),
                shares_an_entity,
                "{} against {}",
                mutation.name,
                projection.name
            );
        }
    }
}

#[test]
fn a_command_affects_exactly_the_projections_that_read_an_entity_it_touches() {
    for command in COMMANDS {
        for projection in PROJECTIONS {
            let shares_an_entity = projection
                .depends_on
                .iter()
                .any(|entity| command.touches.contains(entity));
            assert_eq!(
                REGISTRY.command_affects_projection(command.name, projection.name),
                shares_an_entity,
                "{} against {}",
                command.name,
                projection.name
            );
        }
    }
}

#[test]
fn a_mutation_name_is_not_a_command_and_a_command_name_is_not_a_mutation() {
    for mutation in MUTATIONS {
        assert!(REGISTRY.command(mutation.name).is_none());
    }
    for command in COMMANDS {
        assert!(REGISTRY.mutation(command.name).is_none());
    }
}
