use clerkenwell_schema::{GeneratedMutationSpec, GeneratedProjectionSpec};

/// The projections and mutations an application serves, by wire name.
#[derive(Debug)]
pub struct ProjectionRegistry {
    projections: &'static [GeneratedProjectionSpec],
    mutations: &'static [GeneratedMutationSpec],
}

impl ProjectionRegistry {
    pub const fn new(
        projections: &'static [GeneratedProjectionSpec],
        mutations: &'static [GeneratedMutationSpec],
    ) -> Self {
        Self {
            projections,
            mutations,
        }
    }

    pub fn projections(&self) -> &'static [GeneratedProjectionSpec] {
        self.projections
    }

    pub fn mutations(&self) -> &'static [GeneratedMutationSpec] {
        self.mutations
    }

    pub fn projection(&self, name: &str) -> Option<&'static GeneratedProjectionSpec> {
        self.projections
            .iter()
            .find(|projection| projection.name == name)
    }

    pub fn mutation(&self, name: &str) -> Option<&'static GeneratedMutationSpec> {
        self.mutations.iter().find(|mutation| mutation.name == name)
    }

    pub fn projection_names(&self) -> Vec<&'static str> {
        self.projections
            .iter()
            .map(|projection| projection.name)
            .collect()
    }

    pub fn mutation_names(&self) -> Vec<&'static str> {
        self.mutations
            .iter()
            .map(|mutation| mutation.name)
            .collect()
    }

    /// Whether a mutation can change a projection: whether it writes an
    /// entity the projection reads. Unknown names affect nothing.
    pub fn mutation_affects_projection(&self, mutation: &str, projection: &str) -> bool {
        let (Some(mutation), Some(projection)) =
            (self.mutation(mutation), self.projection(projection))
        else {
            return false;
        };
        projection
            .depends_on
            .iter()
            .any(|entity| mutation.touches.contains(entity))
    }
}
