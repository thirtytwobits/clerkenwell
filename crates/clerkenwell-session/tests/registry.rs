use clerkenwell_schema::{
    GeneratedMaterializationPlan, GeneratedMutationSpec, GeneratedProjectionSpec,
};
use clerkenwell_session::ProjectionRegistry;

static PROJECTIONS: &[GeneratedProjectionSpec] = &[
    GeneratedProjectionSpec {
        name: "notes.list",
        depends_on: &["Note"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
    GeneratedProjectionSpec {
        name: "boards.list",
        depends_on: &["Board"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
];
static MUTATIONS: &[GeneratedMutationSpec] = &[
    GeneratedMutationSpec {
        name: "note.delete",
        touches: &["Note"],
    },
    GeneratedMutationSpec {
        name: "board.pin",
        touches: &["Board", "Note"],
    },
];
static REGISTRY: ProjectionRegistry = ProjectionRegistry::new(PROJECTIONS, MUTATIONS);

#[test]
fn every_registered_name_resolves_to_its_spec() {
    for projection in PROJECTIONS {
        assert_eq!(REGISTRY.projection(projection.name), Some(projection));
    }
    for mutation in MUTATIONS {
        assert_eq!(REGISTRY.mutation(mutation.name), Some(mutation));
    }
    assert_eq!(REGISTRY.projection_names().len(), PROJECTIONS.len());
    assert_eq!(REGISTRY.mutation_names().len(), MUTATIONS.len());
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
        assert!(!REGISTRY.mutation_affects_projection(&candidate, "notes.list"));
        assert!(!REGISTRY.mutation_affects_projection("note.delete", &candidate));
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
