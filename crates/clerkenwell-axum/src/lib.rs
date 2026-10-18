//! A reference transport for the projection session protocol: JSON-RPC 2.0
//! over an axum WebSocket.
//!
//! The application authenticates each connection as a principal; every
//! request on it is made as that principal. Each connection keeps its own
//! subscriptions, and takes every publication in the order it was made: the
//! change each mutation or command makes, and each change the application
//! announces or its stores make. A connection that falls behind takes a
//! snapshot for each of its subscriptions and skips what it missed.
//!
//! A connection serves every request in the order it arrives, except that a
//! command runs while the connection serves its other requests and updates.
//! The connection answers a command at its command's place among the
//! publications, once its host has returned, so its caller takes every change
//! published before it first. A command still running when its connection
//! closes runs on, and its change reaches every other connection.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use clerkenwell_events::{ChangeEvent, Principal};
use clerkenwell_session::transport::{
    ProjectionCommandRequest, ProjectionErrorCode, ProjectionErrorEnvelope, ProjectionOperation,
    ProjectionTransportEvent, PROJECTION_UPDATE_NOTIFICATION,
};
use clerkenwell_session::{
    command_reply, decode_request, deliver_announcement, deliver_change, publish, publish_command,
    resync_all, run_command, serve, Accepted, Announced, Held, ProjectionFailure, ProjectionHost,
    ProjectionRefusal, ProjectionReply, ProjectionRequest, ProjectionSubscriptions, RanCommand,
};
use futures_util::{FutureExt, SinkExt, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, Semaphore};

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

/// A refused request, as a JSON-RPC error answers it.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcFailure {
    refusal: ProjectionRefusal,
    envelope: Option<Box<ProjectionErrorEnvelope>>,
}

impl RpcFailure {
    pub fn new(
        code: ProjectionErrorCode,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        Self::refused(ProjectionRefusal::new(code, message, details))
    }

    /// The JSON-RPC error object: its code, its message, and the refusal's
    /// details with its protocol code and envelope.
    fn error_object(&self) -> ErrorObject {
        let mut details = match &self.refusal.details {
            Some(Value::Object(details)) => details.clone(),
            Some(other) => Map::from_iter([("payload".to_owned(), other.clone())]),
            None => Map::new(),
        };
        details.remove("code");
        details.remove("projection_error");
        ErrorObject {
            code: rpc_code(self.refusal.code),
            message: self.refusal.message.clone(),
            data: Some(ErrorData {
                code: self.refusal.code,
                projection_error: self.envelope.as_deref().cloned(),
                details,
            }),
        }
    }
}

impl ProjectionFailure for RpcFailure {
    fn refused(refusal: ProjectionRefusal) -> Self {
        Self {
            refusal,
            envelope: None,
        }
    }

    fn describe(&self) -> ProjectionRefusal {
        self.refusal.clone()
    }

    fn described(mut self, envelope: ProjectionErrorEnvelope) -> Self {
        self.envelope = Some(Box::new(envelope));
        self
    }
}

/// The JSON-RPC error code a refusal answers with.
fn rpc_code(code: ProjectionErrorCode) -> i64 {
    match code {
        ProjectionErrorCode::UnknownMutation
        | ProjectionErrorCode::UnsupportedMutation
        | ProjectionErrorCode::UnknownCommand
        | ProjectionErrorCode::UnsupportedCommand
        | ProjectionErrorCode::UnknownProjection
        | ProjectionErrorCode::UnsupportedProjection
        | ProjectionErrorCode::InvalidParams
        | ProjectionErrorCode::ValidationFailed => -32602,
        ProjectionErrorCode::NotFound => -32004,
        ProjectionErrorCode::Conflict
        | ProjectionErrorCode::StaleWrite
        | ProjectionErrorCode::BaseRevisionInFuture => -32009,
        ProjectionErrorCode::RateLimit | ProjectionErrorCode::InternalError => -32000,
    }
}

/// The JSON-RPC version every frame names.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
enum Version {
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Request {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// The answer to a request that has an id.
#[derive(Debug, Serialize, JsonSchema)]
struct Response<R> {
    jsonrpc: Version,
    id: Value,
    #[serde(flatten)]
    outcome: Outcome<R>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Outcome<R> {
    Result(R),
    Error(ErrorObject),
}

#[derive(Debug, Serialize, JsonSchema)]
struct ErrorObject {
    code: i64,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<ErrorData>,
}

impl ErrorObject {
    /// An error the protocol layer raises before any request is served.
    fn framing(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// A refusal's details, with its protocol code and envelope.
#[derive(Debug, Serialize, JsonSchema)]
struct ErrorData {
    code: ProjectionErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projection_error: Option<ProjectionErrorEnvelope>,
    #[serde(flatten)]
    details: Map<String, Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Notification<'a, T> {
    jsonrpc: Version,
    method: &'static str,
    params: &'a T,
}

/// What reaches every connection, in the order it happened.
enum Publication<M, C, N> {
    /// The change a request one connection served made, for every other
    /// connection.
    Served { origin: u64, change: Accepted<M, C> },
    /// A command one connection ran, once its host returned: every connection
    /// publishes its change, and its own connection answers it.
    Ran {
        origin: u64,
        ticket: u64,
        outcome: Result<RanCommand<C>, RpcFailure>,
    },
    /// A change the application announced.
    Announced(Arc<Announced<N>>),
    /// A change one of the application's stores made.
    Stored(Box<ChangeEvent>),
}

/// A publication and its place in the order every connection takes them in.
struct Sent<P> {
    sequence: u64,
    publication: P,
}

/// Every connection's publications, numbered in the order they are sent.
struct Publications<P> {
    sender: broadcast::Sender<Arc<Sent<P>>>,
    /// The number of the last publication sent, held while one is sent.
    last: Mutex<u64>,
}

impl<P> Publications<P> {
    fn new(window: NonZeroUsize) -> Self {
        Self {
            sender: broadcast::channel(window.get()).0,
            last: Mutex::new(0),
        }
    }

    /// Sends `publication`, first handing it to `numbered` once it has its
    /// number and before a later subscriber starts after it.
    fn send(&self, publication: P, numbered: impl FnOnce(&Arc<Sent<P>>)) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        *last += 1;
        let sent = Arc::new(Sent {
            sequence: *last,
            publication,
        });
        numbered(&sent);
        // No connection is listening when the send fails.
        let _ = self.sender.send(sent);
    }

    /// A receiver of every publication sent from now on, and the number of the
    /// last one it does not receive.
    fn subscribe(&self) -> (u64, broadcast::Receiver<Arc<Sent<P>>>) {
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        (*last, self.sender.subscribe())
    }
}

type PublicationOf<A> = Publication<
    <A as ProjectionHost>::MutationResult,
    <A as ProjectionHost>::CommandResult,
    <A as ProjectionHost>::Announcement,
>;

/// Each command a connection ran, by its ticket, until the connection answers
/// it.
type RanCommands<A> = Arc<Mutex<HashMap<u64, Arc<Sent<PublicationOf<A>>>>>>;

type Subscriptions<A> = Mutex<
    ProjectionSubscriptions<<A as ProjectionHost>::Patch, Held<<A as ProjectionHost>::Delivery>>,
>;

type Reply<A> = ProjectionReply<
    <A as ProjectionHost>::Snapshot,
    <A as ProjectionHost>::Patch,
    <A as ProjectionHost>::MutationResult,
    <A as ProjectionHost>::CommandResult,
>;

type Event<A> =
    ProjectionTransportEvent<<A as ProjectionHost>::Snapshot, <A as ProjectionHost>::Patch>;

/// What a request frame asks of its connection.
enum Received {
    /// The frames that answer it.
    Answered(Vec<String>),
    /// A command to run while the connection serves its other requests, and
    /// the id its response answers.
    Command {
        id: Option<Value>,
        request: ProjectionCommandRequest,
    },
}

/// Serves an application's projections to every connection.
pub struct ProjectionServer<A: ProjectionHost> {
    application: A,
    publications: Arc<Publications<PublicationOf<A>>>,
    /// A permit for each command that may run at once, across every
    /// connection.
    commands: Arc<Semaphore>,
    connections: AtomicU64,
}

impl<A> ProjectionServer<A>
where
    A: ProjectionHost<Failure = RpcFailure> + Send + 'static,
    A::Snapshot: 'static,
    A::Patch: Sync + 'static,
    A::Delivery: Sync + 'static,
    A::MutationResult: 'static,
    A::CommandResult: 'static,
    A::Announcement: 'static,
{
    /// A server whose connections resynchronise once they fall more than
    /// `publication_window` publications behind: mutations, commands, the
    /// application's announcements and its stores' changes. At most
    /// `command_limit` commands run at once, across every connection; a
    /// command beyond them is refused as `rate_limit`.
    pub fn new(
        application: A,
        publication_window: NonZeroUsize,
        command_limit: NonZeroUsize,
    ) -> Arc<Self> {
        let publications = Arc::new(Publications::new(publication_window));
        if let Some(announcements) = application.announcements() {
            let publications = Arc::clone(&publications);
            announcements.listen(move |announced| {
                publications.send(Publication::Announced(Arc::clone(announced)), |_| {});
            });
        }
        if let Some(authoring) = application.authoring_states() {
            let publications = Arc::clone(&publications);
            authoring.stores().changes().listen(move |change| {
                publications.send(Publication::Stored(Box::new(change.clone())), |_| {});
            });
        }
        Arc::new(Self {
            application,
            publications,
            commands: Arc::new(Semaphore::new(command_limit.get())),
            connections: AtomicU64::new(1),
        })
    }

    pub fn application(&self) -> &A {
        &self.application
    }

    /// A router that serves the protocol to WebSocket connections at `path`.
    /// `authenticate` names the principal a connection's upgrade request
    /// authenticates as, from the request's parts: its headers, its URI, and
    /// the extensions the application's middleware set. A request it names
    /// none for is refused as unauthorised.
    ///
    /// The route captures the server instead of taking it as axum `State`,
    /// which code scanning reads as request input and follows into the
    /// store's paths. The handler's parameters are the request's parts,
    /// which only `authenticate` reads, and the client's upgrade.
    pub fn router(
        self: Arc<Self>,
        path: &str,
        authenticate: impl Fn(&Parts) -> Option<Principal> + Clone + Send + Sync + 'static,
    ) -> Router {
        Router::new().route(
            path,
            get(move |parts: Parts, upgrade: WebSocketUpgrade| async move {
                match authenticate(&parts) {
                    Some(principal) => upgrade
                        .on_upgrade(move |socket| self.serve_socket(socket, principal))
                        .into_response(),
                    None => StatusCode::UNAUTHORIZED.into_response(),
                }
            }),
        )
    }

    /// Serves one WebSocket, authenticated as `principal`, until it closes.
    pub async fn serve_socket(self: Arc<Self>, socket: WebSocket, principal: Principal) {
        let origin = self.connections.fetch_add(1, Ordering::Relaxed);
        let subscriptions: Subscriptions<A> =
            Mutex::new(ProjectionSubscriptions::new(principal.clone()));
        let (_, mut published) = self.publications.subscribe();
        // The id each running command's response answers, by its ticket.
        let mut running: HashMap<u64, Option<Value>> = HashMap::new();
        let ran: RanCommands<A> = Arc::default();
        let mut tickets = 0;
        let (mut sink, mut stream) = socket.split();
        'serving: loop {
            let frames = tokio::select! {
                message = stream.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        match self.receive(origin, &subscriptions, text.as_str()).await {
                            Received::Answered(frames) => frames,
                            Received::Command { id, request } => {
                                tickets += 1;
                                match self.start(origin, tickets, &principal, request, &ran) {
                                    Ok(()) => {
                                        running.insert(tickets, id);
                                        continue;
                                    }
                                    Err(failure) => Self::answer(id, Err(failure.error_object()), &[]),
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                },
                sent = published.recv() => match sent {
                    Ok(sent) => self.take(origin, &subscriptions, &sent, &mut running, &ran).await,
                    Err(RecvError::Lagged(missed)) => {
                        subscriptions
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .record_dropped_updates(missed);
                        let (skipped_through, receiver) = self.publications.subscribe();
                        published = receiver;
                        let mut frames: Vec<String> =
                            resync_all(&self.application, &subscriptions, "broadcastLag")
                                .await
                                .iter()
                                .filter_map(notification)
                                .collect();
                        frames.extend(self.answer_skipped(
                            &subscriptions,
                            skipped_through,
                            &mut running,
                            &ran,
                        ));
                        frames
                    }
                    Err(RecvError::Closed) => break,
                },
            };
            for frame in frames {
                if sink.send(Message::Text(frame.into())).await.is_err() {
                    break 'serving;
                }
            }
        }
    }

    /// Starts the command `request` names as `ticket` of the connection
    /// `origin`, authenticated as `principal`, unless as many commands as the
    /// server allows are running. Once its host returns, its outcome is
    /// published, and kept in `ran` until its connection answers it.
    fn start(
        self: &Arc<Self>,
        origin: u64,
        ticket: u64,
        principal: &Principal,
        request: ProjectionCommandRequest,
        ran: &RanCommands<A>,
    ) -> Result<(), RpcFailure> {
        let Ok(permit) = Arc::clone(&self.commands).try_acquire_owned() else {
            return Err(command_refusal(
                ProjectionErrorCode::RateLimit,
                "The server is running as many commands as it allows.",
                &request.command,
            ));
        };
        let server = Arc::clone(self);
        let principal = principal.clone();
        let ran = Arc::clone(ran);
        tokio::spawn(async move {
            let command = request.command.clone();
            let outcome = AssertUnwindSafe(run_command(&server.application, &principal, request))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(command_refusal(
                        ProjectionErrorCode::InternalError,
                        format!("The command \"{command}\" ended without a result."),
                        &command,
                    ))
                });
            drop(permit);
            let publication = Publication::Ran {
                origin,
                ticket,
                outcome,
            };
            server.publications.send(publication, |sent| {
                ran.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(ticket, Arc::clone(sent));
            });
        });
        Ok(())
    }

    /// The frames one publication sends the connection `origin`: the updates
    /// it brings its subscriptions, after the response to a command of its
    /// own that it carries.
    async fn take(
        &self,
        origin: u64,
        subscriptions: &Subscriptions<A>,
        sent: &Sent<PublicationOf<A>>,
        running: &mut HashMap<u64, Option<Value>>,
        ran: &RanCommands<A>,
    ) -> Vec<String> {
        let events = match &sent.publication {
            Publication::Served { origin: from, .. } if *from == origin => Vec::new(),
            Publication::Served { change, .. } => {
                publish(&self.application, subscriptions, change).await.1
            }
            Publication::Announced(announced) => {
                deliver_announcement(&self.application, subscriptions, announced).await
            }
            Publication::Stored(change) => {
                deliver_change(&self.application, subscriptions, change).await
            }
            Publication::Ran {
                origin: from,
                ticket,
                outcome,
            } if *from == origin => {
                ran.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(ticket);
                let id = running.remove(ticket).flatten();
                return match outcome {
                    Ok(command) => {
                        let (revision, events) =
                            publish_command(&self.application, subscriptions, command).await;
                        let response = command_reply::<A>(command, revision)
                            .map_err(|failure| failure.error_object());
                        Self::answer(id, response, &events)
                    }
                    Err(failure) => Self::answer(id, Err(failure.error_object()), &[]),
                };
            }
            Publication::Ran {
                outcome: Ok(command),
                ..
            } => {
                publish_command(&self.application, subscriptions, command)
                    .await
                    .1
            }
            Publication::Ran { .. } => Vec::new(),
        };
        events.iter().filter_map(notification).collect()
    }

    /// The responses to the connection's commands whose publications it
    /// skipped, through `skipped_through`, after it fell behind. The snapshots
    /// it was resent hold their changes.
    fn answer_skipped(
        &self,
        subscriptions: &Subscriptions<A>,
        skipped_through: u64,
        running: &mut HashMap<u64, Option<Value>>,
        ran: &RanCommands<A>,
    ) -> Vec<String> {
        let mut skipped: Vec<Arc<Sent<PublicationOf<A>>>> = {
            let mut ran = ran.lock().unwrap_or_else(PoisonError::into_inner);
            let tickets: Vec<u64> = ran
                .iter()
                .filter(|(_, sent)| sent.sequence <= skipped_through)
                .map(|(ticket, _)| *ticket)
                .collect();
            tickets
                .iter()
                .filter_map(|ticket| ran.remove(ticket))
                .collect()
        };
        skipped.sort_by_key(|sent| sent.sequence);
        let revision = subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .change_revision(false);
        let mut frames = Vec::new();
        for sent in skipped {
            let Publication::Ran {
                ticket, outcome, ..
            } = &sent.publication
            else {
                continue;
            };
            let response = match outcome {
                Ok(command) => command_reply::<A>(command, revision),
                Err(failure) => Err(failure.clone()),
            };
            let id = running.remove(ticket).flatten();
            frames.extend(Self::answer(
                id,
                response.map_err(|failure| failure.error_object()),
                &[],
            ));
        }
        frames
    }

    /// What one request frame asks of its connection.
    async fn receive(&self, origin: u64, subscriptions: &Subscriptions<A>, text: &str) -> Received {
        let request = match serde_json::from_str::<Request>(text) {
            Ok(request) => request,
            Err(error) => {
                return Received::Answered(vec![frame(
                    Value::Null,
                    Err::<Value, _>(ErrorObject::framing(PARSE_ERROR, error.to_string())),
                )])
            }
        };
        let id = request.id.clone();
        if request.jsonrpc != "2.0" {
            return Received::Answered(vec![frame(
                id.unwrap_or(Value::Null),
                Err::<Value, _>(ErrorObject::framing(
                    INVALID_REQUEST,
                    "Only JSON-RPC 2.0 requests are served.",
                )),
            )]);
        }
        let served = match decode_request::<RpcFailure>(&request.method, request.params) {
            None => {
                let error = ErrorObject::framing(
                    METHOD_NOT_FOUND,
                    format!("Unknown method \"{}\".", request.method),
                );
                return Received::Answered(Self::answer(id, Err(error), &[]));
            }
            Some(Ok(ProjectionRequest::Command(request))) => {
                return Received::Command { id, request };
            }
            Some(Ok(request)) => serve(&self.application, subscriptions, request).await,
            Some(Err(failure)) => Err(failure),
        };
        Received::Answered(self.respond(origin, id, served))
    }

    /// The frames that answer a served request: its response, when it has an
    /// id, then the updates it sends. The change it made goes to every other
    /// connection.
    fn respond(
        &self,
        origin: u64,
        id: Option<Value>,
        served: Result<Reply<A>, RpcFailure>,
    ) -> Vec<String> {
        match served {
            Err(failure) => Self::answer(id, Err(failure.error_object()), &[]),
            Ok(reply) => {
                if let Some(change) = reply.accepted {
                    self.publications
                        .send(Publication::Served { origin, change }, |_| {});
                }
                Self::answer(id, Ok(reply.result), &reply.events)
            }
        }
    }

    /// The frames that answer a request: its response, when it has an id,
    /// then `events`.
    fn answer(
        id: Option<Value>,
        response: Result<Value, ErrorObject>,
        events: &[Event<A>],
    ) -> Vec<String> {
        id.map(|id| frame(id, response))
            .into_iter()
            .chain(events.iter().filter_map(notification))
            .collect()
    }
}

/// The refusal of the command `command`, addressed to it.
fn command_refusal(
    code: ProjectionErrorCode,
    message: impl Into<String>,
    command: &str,
) -> RpcFailure {
    let message = message.into();
    RpcFailure::new(code, message.clone(), None).described(ProjectionErrorEnvelope::new(
        code,
        message,
        ProjectionOperation::Command,
        Some(command),
        None,
    ))
}

/// The response frame that answers the request `id`.
fn frame<R: Serialize>(id: Value, outcome: Result<R, ErrorObject>) -> String {
    let outcome = match outcome {
        Ok(result) => Outcome::Result(result),
        Err(error) => Outcome::Error(error),
    };
    serde_json::to_string(&Response {
        jsonrpc: Version::V2,
        id,
        outcome,
    })
    .expect("a response frame serialises")
}

/// An update as the notification that sends it. An update that cannot be
/// encoded is not sent: the subscription's next update then does not follow
/// what its client holds, and the client resynchronises.
fn notification<S: Serialize, P: Serialize>(
    event: &ProjectionTransportEvent<S, P>,
) -> Option<String> {
    serde_json::to_string(&Notification {
        jsonrpc: Version::V2,
        method: PROJECTION_UPDATE_NOTIFICATION,
        params: event,
    })
    .ok()
}

/// The JSON-RPC framing, as `wire-protocol.json` records it.
#[cfg(feature = "testing")]
pub mod testing {
    use schemars::generate::{SchemaGenerator, SchemaSettings};
    use schemars::JsonSchema;
    use serde_json::{json, Map, Value};

    use clerkenwell_session::testing::strip_annotations;
    use clerkenwell_session::transport::ProjectionErrorCode;

    use super::{
        rpc_code, Notification, Request, Response, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
    };

    pub use clerkenwell_session::testing::WIRE_PROTOCOL_RECORD;

    /// The JSON Schema of the frames that carry the session protocol, and the
    /// JSON-RPC error code each failure answers with. The session protocol's
    /// own record holds the parameters and results the frames carry.
    pub fn wire_protocol() -> Value {
        let mut generator = SchemaSettings::draft2020_12().into_generator();
        let request = schema::<Request>(&mut generator);
        let response = schema::<Response<Value>>(&mut generator);
        let notification = schema::<Notification<'static, Value>>(&mut generator);
        let refusals: Map<String, Value> = ProjectionErrorCode::ALL
            .into_iter()
            .map(|code| (code.as_str().to_owned(), json!(rpc_code(code))))
            .collect();
        let mut definitions = generator.take_definitions(true);
        definitions.values_mut().for_each(strip_annotations);
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "request": request,
            "response": response,
            "notification": notification,
            "error_codes": {
                "parse_error": PARSE_ERROR,
                "invalid_request": INVALID_REQUEST,
                "method_not_found": METHOD_NOT_FOUND,
                "refusals": refusals,
            },
            "$defs": definitions,
        })
    }

    fn schema<T: JsonSchema>(generator: &mut SchemaGenerator) -> Value {
        let mut schema = generator.subschema_for::<T>().to_value();
        strip_annotations(&mut schema);
        schema
    }
}
