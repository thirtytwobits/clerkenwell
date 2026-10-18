use clerkenwell_schema::{
    GeneratedCommandSpec, GeneratedEntityAuthoringSpec, GeneratedMutationSpec,
    GeneratedProjectionSpec,
};

/// The projections, mutations and commands an application serves, by wire
/// name, and how each entity is authored.
#[derive(Debug)]
pub struct ProjectionRegistry {
    projections: &'static [GeneratedProjectionSpec],
    mutations: &'static [GeneratedMutationSpec],
    commands: &'static [GeneratedCommandSpec],
    authoring: &'static [GeneratedEntityAuthoringSpec],
}

impl ProjectionRegistry {
    pub const fn new(
        projections: &'static [GeneratedProjectionSpec],
        mutations: &'static [GeneratedMutationSpec],
        commands: &'static [GeneratedCommandSpec],
        authoring: &'static [GeneratedEntityAuthoringSpec],
    ) -> Self {
        Self {
            projections,
            mutations,
            commands,
            authoring,
        }
    }

    pub fn projections(&self) -> &'static [GeneratedProjectionSpec] {
        self.projections
    }

    pub fn mutations(&self) -> &'static [GeneratedMutationSpec] {
        self.mutations
    }

    pub fn commands(&self) -> &'static [GeneratedCommandSpec] {
        self.commands
    }

    pub fn projection(&self, name: &str) -> Option<&'static GeneratedProjectionSpec> {
        self.projections
            .iter()
            .find(|projection| projection.name == name)
    }

    pub fn mutation(&self, name: &str) -> Option<&'static GeneratedMutationSpec> {
        self.mutations.iter().find(|mutation| mutation.name == name)
    }

    pub fn command(&self, name: &str) -> Option<&'static GeneratedCommandSpec> {
        self.commands.iter().find(|command| command.name == name)
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

    pub fn command_names(&self) -> Vec<&'static str> {
        self.commands.iter().map(|command| command.name).collect()
    }

    /// Whether a mutation only plans a change: it writes nothing, so no
    /// subscriber sees it.
    pub fn plans_only(&self, mutation: &str) -> bool {
        self.authoring
            .iter()
            .any(|entity| entity.planning_mutations.contains(&mutation))
    }

    /// Whether a mutation can change a projection: whether it writes an
    /// entity the projection reads. Unknown names affect nothing.
    pub fn mutation_affects_projection(&self, mutation: &str, projection: &str) -> bool {
        let Some(mutation) = self.mutation(mutation) else {
            return false;
        };
        self.entities_affect_projection(mutation.touches, projection)
    }

    /// Whether a command can change a projection: whether it touches an
    /// entity the projection reads. Unknown names affect nothing.
    pub fn command_affects_projection(&self, command: &str, projection: &str) -> bool {
        let Some(command) = self.command(command) else {
            return false;
        };
        self.entities_affect_projection(command.touches, projection)
    }

    /// Whether a change to the entities `touches` names can change a
    /// projection: whether the projection reads one of them. Unknown names
    /// affect nothing.
    pub fn entities_affect_projection(&self, touches: &[&str], projection: &str) -> bool {
        self.projection(projection).is_some_and(|projection| {
            projection
                .depends_on
                .iter()
                .any(|entity| touches.contains(entity))
        })
    }
}
