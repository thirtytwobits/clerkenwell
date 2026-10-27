//! One operation per projection, mutation and command of an application's
//! definition, run by its server.

use std::collections::BTreeMap;
use std::sync::Arc;

use clapham::{
    Call, Category, Contribution, Effect, Handler, HandlerFuture, Locality, Operation, Outcome,
    Refusal,
};
use clerkenwell_axum::ProjectionServer;
use clerkenwell_schema::GeneratedTransportValue;
use clerkenwell_session::AuthoringStates;
use futures_util::future::{select, Either};
use schemars::Schema;
use serde_json::{json, Map, Value};

use crate::host::{GeneratedHost, OperationsHost};
use crate::refusals::{self, host_refusals, CALLER_REFUSALS};
use crate::{principal, ContributionError};

/// What a generated operation runs.
#[derive(Clone, Copy)]
enum Entry {
    Projection,
    Mutation,
    Command,
}

/// One operation per projection, mutation and command of `server`'s
/// registry, under its namespace. A collaborative entity's authoring state
/// and import mutation carry encoded replica updates, so neither is an
/// operation.
pub(crate) fn contribute<A: GeneratedHost>(
    server: Arc<ProjectionServer<A>>,
    host: Arc<dyn OperationsHost>,
) -> Result<Contribution, ContributionError> {
    let application = server.application();
    let registry = application.registry();
    let namespace = registry.namespace();
    let authoring = application.authoring_states();
    let replicated =
        |projection: &str| authoring.is_some_and(|states| states.plan(projection).is_some());
    let imports = |mutation: &str, touches: &[&str]| {
        touches.iter().any(|entity| {
            authoring
                .and_then(|states: &AuthoringStates| states.entity(entity))
                .is_some_and(|plan| plan.import_mutation == mutation)
        })
    };
    let mut contribution = Contribution::new(namespace);
    for spec in registry
        .projections()
        .iter()
        .filter(|spec| !replicated(spec.name))
    {
        let operation = descriptor(
            namespace,
            spec.name,
            spec.description,
            Effect::Query,
            spec.params_schema,
            "snapshot",
            spec.snapshot_schema,
        )?;
        contribution = contribution.operation(
            operation,
            Generated::new(Entry::Projection, spec.name, &server, &host),
        );
    }
    for spec in registry
        .mutations()
        .iter()
        .filter(|spec| !imports(spec.name, spec.touches))
    {
        let operation = descriptor(
            namespace,
            spec.name,
            spec.description,
            Effect::Mutation,
            spec.params_schema,
            "result",
            spec.result_schema,
        )?;
        contribution = contribution.operation(
            operation,
            Generated::new(Entry::Mutation, spec.name, &server, &host),
        );
    }
    for spec in registry.commands() {
        let effect = if spec.touches.is_empty() {
            Effect::Query
        } else {
            Effect::Mutation
        };
        let operation = descriptor(
            namespace,
            spec.name,
            spec.description,
            effect,
            spec.params_schema,
            "result",
            spec.result_schema,
        )?;
        contribution = contribution.operation(
            operation,
            Generated::new(Entry::Command, spec.name, &server, &host),
        );
    }
    Ok(contribution)
}

/// The descriptor of the entry `name`, whose input is its parameter schema
/// and whose output holds the number of the modification it reads or saved
/// and, under `field`, a value of `schema`.
fn descriptor(
    namespace: &str,
    name: &str,
    description: &str,
    effect: Effect,
    params_schema: &str,
    field: &str,
    schema: &str,
) -> Result<Operation, ContributionError> {
    let complete = format!("{namespace}.{name}");
    let invalid = |reason: String| ContributionError {
        operation: complete.clone(),
        reason,
    };
    let input: Map<String, Value> = serde_json::from_str(params_schema)
        .map_err(|error| invalid(format!("its parameter schema is not an object: {error}")))?;
    let mut value: Map<String, Value> = serde_json::from_str(schema)
        .map_err(|error| invalid(format!("its {field} schema is not an object: {error}")))?;
    // References resolve against the output's root, which holds the value's
    // definitions.
    let definitions = value.remove("$defs");
    let mut output = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["modification", field],
        "properties": {
            "modification": {
                "description": "The number of the last modification it reads or saved; null when it names none.",
                "type": ["integer", "null"],
                "minimum": 0,
            },
            field: value,
        },
    });
    if let Some(definitions) = definitions {
        output["$defs"] = definitions;
    }
    let scope = match effect {
        Effect::Query => format!("{namespace}:read"),
        _ => format!("{namespace}:write"),
    };
    let refusals: BTreeMap<String, Category> = host_refusals()
        .chain(CALLER_REFUSALS.iter().copied())
        .map(|(code, category)| (code.to_owned(), category))
        .collect();
    Ok(Operation {
        name: complete
            .parse()
            .map_err(|_| invalid("it is not a Clapham operation name".to_owned()))?,
        title: None,
        description: description.to_owned(),
        input,
        output: Some(
            Schema::try_from(output)
                .map_err(|error| invalid(format!("its output schema is invalid: {error}")))?,
        ),
        effect,
        scopes: vec![scope],
        policies: Vec::new(),
        locality: Locality::Daemon,
        refusals,
        streams: false,
        preview: false,
        positional: Vec::new(),
        confirmation: false,
        idempotent: matches!(effect, Effect::Query),
        open_world: false,
    })
}

/// The handler of one generated operation.
struct Generated<A: GeneratedHost> {
    entry: Entry,
    name: &'static str,
    server: Arc<ProjectionServer<A>>,
    host: Arc<dyn OperationsHost>,
}

impl<A: GeneratedHost> Generated<A> {
    fn new(
        entry: Entry,
        name: &'static str,
        server: &Arc<ProjectionServer<A>>,
        host: &Arc<dyn OperationsHost>,
    ) -> Self {
        Self {
            entry,
            name,
            server: Arc::clone(server),
            host: Arc::clone(host),
        }
    }

    async fn run(&self, call: Call) -> Result<Outcome, Refusal> {
        let actor = principal(&*self.host, &call)?;
        let name = self.name;
        let (field, modification, value) = match self.entry {
            Entry::Projection => {
                let numbered = self
                    .server
                    .application()
                    .snapshot(name, &call.input, None)
                    .await
                    .map_err(refusals::host)?;
                let value = numbered
                    .snapshot
                    .value()
                    .map_err(|error| refusals::unencodable(name, error))?;
                ("snapshot", Some(numbered.through), value)
            }
            Entry::Mutation => {
                let made = self
                    .server
                    .mutate(&actor, name, call.input, |result| result.value())
                    .await
                    .map_err(refusals::host)?;
                ("result", made.modification, made.result)
            }
            Entry::Command => {
                let running = self
                    .server
                    .command(&actor, name, call.input, |result| result.value());
                let cancelled = call.cancel.cancelled();
                futures_util::pin_mut!(running, cancelled);
                match select(running, cancelled).await {
                    Either::Left((ran, _)) => {
                        let ran = ran.map_err(refusals::host)?;
                        ("result", ran.modification, ran.result)
                    }
                    Either::Right(((), _)) => return Err(refusals::cancelled_while_running(name)),
                }
            }
        };
        let mut result = json!({ "modification": modification });
        result[field] = value;
        Ok(Outcome::new(result))
    }
}

impl<A: GeneratedHost> Handler for Generated<A> {
    fn call(&self, call: Call) -> HandlerFuture<'_> {
        Box::pin(self.run(call))
    }
}
