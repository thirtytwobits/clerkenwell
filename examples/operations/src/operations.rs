//! Clerkenwell's operations over the tasks, contributed to Clapham.
//!
//! A grant acts as the principal its subject names, of the kind its
//! `actor_kind` claim names; a grant without one acts as no principal. Every
//! task but `task-private` is open to every grant; `task-private` only to
//! `owner`'s.

use std::future::{ready, Ready};
use std::sync::Arc;

use clapham::{Configuration, Exposure, OAuthContext};
use clerkenwell_axum::ProjectionServer;
use clerkenwell_events::{ActorKind, Principal};
use clerkenwell_ops::{document_policies, DocumentScope, Forbidden, Invalid, OperationsHost};
use serde_json::Value;

use crate::{validate, Tasks, COMMAND_LIMIT, PUBLICATION_WINDOW};

/// The issuer every grant the tasks accept is verified as issued by.
pub const ISSUER: &str = "https://identity.example.com";

/// The audience every grant the tasks accept is issued for.
pub const AUDIENCE: &str = "https://tasks.example.com";

/// The claim naming the kind of principal a grant acts as.
pub const ACTOR_KIND: &str = "actor_kind";

/// The task only `owner` may read or edit.
pub const PRIVATE_TASK: &str = "task-private";

/// The tasks the store holds before any call: each id and title.
pub const SEEDED: &[(&str, &str)] = &[
    ("task-1", "Write the report"),
    ("task-2", "Read the paper"),
    (PRIVATE_TASK, "Plan the surprise"),
];

/// The tasks' identity binding and validation.
pub struct TasksOperations;

impl OperationsHost for TasksOperations {
    fn principal(&self, auth: &OAuthContext) -> Option<Principal> {
        let kind = match auth.claim(ACTOR_KIND)?.as_str()? {
            "human" => ActorKind::Human,
            "agent" => ActorKind::Agent,
            "service" => ActorKind::Service,
            _ => return None,
        };
        Some(Principal::new(auth.subject(), kind))
    }

    fn validate(&self, _entity: &str, document: &Value) -> Result<(), Invalid> {
        validate(document).map_err(|error| Invalid::new(error.message))
    }
}

/// Refuses every grant but `owner`'s the private task.
pub fn check(auth: OAuthContext, scope: DocumentScope) -> Ready<Result<(), Forbidden>> {
    ready(
        if scope.id.as_deref() == Some(PRIVATE_TASK) && auth.subject() != "owner" {
            Err(Forbidden::new("Only its owner may reach this task."))
        } else {
            Ok(())
        },
    )
}

/// A server of freshly seeded tasks.
pub fn server() -> Arc<ProjectionServer<Tasks>> {
    ProjectionServer::new(Tasks::seeded(SEEDED), PUBLICATION_WINDOW, COMMAND_LIMIT)
}

/// The configuration that exposes every operation Clerkenwell contributes
/// over `server`, with the document policies registered.
pub fn configured(server: Arc<ProjectionServer<Tasks>>) -> Configuration {
    let contributions = clerkenwell_ops::contribute(server, Arc::new(TasksOperations))
        .expect("every entry of the tasks definition is an operation");
    let mut configuration = Configuration::new(ISSUER, AUDIENCE);
    for contribution in contributions {
        let source = contribution.namespace.clone();
        let names: Vec<String> = contribution
            .operations
            .iter()
            .map(|(operation, _)| operation.name.to_string())
            .collect();
        configuration = configuration.provider(source.clone(), contribution);
        for name in names {
            configuration = configuration.expose(Exposure::new(source.clone(), name));
        }
    }
    for (name, policy) in document_policies(check) {
        configuration = configuration.register_policy(name, policy);
    }
    configuration
}
