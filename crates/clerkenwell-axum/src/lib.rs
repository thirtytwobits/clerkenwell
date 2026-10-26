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
//!
//! The application makes a mutation, or runs a command, as a principal with
//! [`ProjectionServer::mutate`] and [`ProjectionServer::command`]. Its change
//! reaches every connection, and the command takes one of the server's
//! command permits as a connection's does.
//!
//! [`ProjectionServer::shut_down`] stops the server in order: it refuses the
//! commands and connections that arrive from then on, lets each running
//! command finish and be answered, then closes every connection.

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
    resync_all, run_command, run_mutation, serve, Accepted, Announced, Held, Modified,
    ProjectionFailure, ProjectionHost, ProjectionRefusal, ProjectionReply, ProjectionRequest,
    ProjectionSubscriptions, RanCommand,
};
use futures_util::{FutureExt, SinkExt, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, oneshot, watch, Semaphore};

/// The origin of a change no connection made. Connections are numbered from
/// 1.
const NO_CONNECTION: u64 = 0;

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
        ProjectionErrorCode::Cancelled => -32002,
        ProjectionErrorCode::Interrupted => -32003,
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
    /// The server is shutting down: every connection closes once it takes
    /// this, after every command its server ran.
    Closing,
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

/// Whether the server is shutting down, and what it waits for when it does.
#[derive(Debug, Default)]
struct Lifecycle {
    stopping: bool,
    /// The commands started and not yet published.
    running: usize,
    /// The connections admitted and not yet ended.
    serving: usize,
}

/// A connection the server counts among those it serves, and every
/// publication sent since it was counted.
struct Admitted<P> {
    published: broadcast::Receiver<Arc<Sent<P>>>,
    serving: Counted,
}

/// Serves an application's projections to every connection.
pub struct ProjectionServer<A: ProjectionHost> {
    application: A,
    publications: Arc<Publications<PublicationOf<A>>>,
    /// A permit for each command that may run at once, across every
    /// connection.
    commands: Arc<Semaphore>,
    connections: AtomicU64,
    lifecycle: Arc<watch::Sender<Lifecycle>>,
    /// The number of the closing publication; 0 until the server shuts down.
    closing_at: Arc<AtomicU64>,
}

/// Holds a place in the lifecycle's count, given back when dropped.
struct Counted {
    lifecycle: Arc<watch::Sender<Lifecycle>>,
    count: fn(&mut Lifecycle) -> &mut usize,
}

impl Counted {
    /// Takes a place in `count`, unless the server is shutting down.
    fn take(
        lifecycle: &Arc<watch::Sender<Lifecycle>>,
        count: fn(&mut Lifecycle) -> &mut usize,
    ) -> Option<Self> {
        let mut taken = false;
        lifecycle.send_if_modified(|state| {
            taken = !state.stopping;
            if taken {
                *count(state) += 1;
            }
            taken
        });
        taken.then(|| Self {
            lifecycle: Arc::clone(lifecycle),
            count,
        })
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        let count = self.count;
        self.lifecycle.send_modify(|state| *count(state) -= 1);
    }
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
            connections: AtomicU64::new(NO_CONNECTION + 1),
            lifecycle: Arc::new(watch::Sender::new(Lifecycle::default())),
            closing_at: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn application(&self) -> &A {
        &self.application
    }

    /// Makes `mutation` with `params` as `principal`, as a mutation a
    /// connection requests is made, and publishes its change to every
    /// connection. The result is the mutation's, as the protocol encodes it.
    pub async fn mutate(
        &self,
        principal: &Principal,
        mutation: &str,
        params: Value,
    ) -> Result<Modified<Value>, RpcFailure> {
        let Modified {
            result,
            modification,
        } = AssertUnwindSafe(run_mutation(&self.application, principal, mutation, params))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                Err(refusal(
                    ProjectionErrorCode::InternalError,
                    "The request ended without a result.",
                    ProjectionOperation::Mutate,
                    Some(mutation),
                ))
            })?;
        let made = Modified {
            result: encoded(&result, ProjectionOperation::Mutate, mutation)?,
            modification,
        };
        let change = Accepted::Mutation {
            mutation: mutation.to_owned(),
            result,
            modification,
        };
        self.publications.send(
            Publication::Served {
                origin: NO_CONNECTION,
                change,
            },
            |_| {},
        );
        Ok(made)
    }

    /// Runs `command` with `params` as `principal`, as a command a connection
    /// requests is run: it takes one of the server's command permits, and is
    /// refused as `rate_limit` beyond them and as `interrupted` once the
    /// server is shutting down. Its outcome is published to every connection.
    /// The command runs on when the caller stops waiting for it.
    pub async fn command(
        self: &Arc<Self>,
        principal: &Principal,
        command: &str,
        params: Value,
    ) -> Result<Modified<Value>, RpcFailure> {
        let (answer, answered) = oneshot::channel();
        let request = ProjectionCommandRequest {
            command: command.to_owned(),
            operation_id: None,
            params,
        };
        let name = command.to_owned();
        // No connection answers it, so it needs no ticket.
        self.start(NO_CONNECTION, 0, principal, request, move |sent| {
            let Publication::Ran { outcome, .. } = &sent.publication else {
                unreachable!("a command's outcome is published as one");
            };
            let _ = answer.send(match outcome {
                Ok(ran) => {
                    encoded(&ran.result, ProjectionOperation::Command, &name).map(|result| {
                        Modified {
                            result,
                            modification: ran.modification,
                        }
                    })
                }
                Err(failure) => Err(failure.clone()),
            });
        })?;
        answered.await.unwrap_or_else(|_| {
            Err(command_refusal(
                ProjectionErrorCode::InternalError,
                format!("The command \"{command}\" ended without a result."),
                command,
            ))
        })
    }

    /// Shuts the server down. From now on it refuses every command as
    /// `interrupted` and every connection's upgrade with `503 Service
    /// Unavailable`. Once each running command has finished and been answered
    /// on its connection, every connection is sent a Close frame and ends.
    /// Resolves when the last connection has ended. The application ends its
    /// own long work as it shuts down, and bounds the wait itself.
    ///
    /// The shutdown goes on when this future is dropped, and every call
    /// resolves once it has ended.
    pub async fn shut_down(&self) {
        let mut begun = false;
        self.lifecycle.send_if_modified(|state| {
            begun = !state.stopping;
            state.stopping = true;
            begun
        });
        if begun {
            let mut lifecycle = self.lifecycle.subscribe();
            let publications = Arc::clone(&self.publications);
            let closing_at = Arc::clone(&self.closing_at);
            tokio::spawn(async move {
                let _ = lifecycle.wait_for(|state| state.running == 0).await;
                publications.send(Publication::Closing, |sent| {
                    closing_at.store(sent.sequence, Ordering::SeqCst);
                });
            });
        }
        let mut lifecycle = self.lifecycle.subscribe();
        let _ = lifecycle
            .wait_for(|state| state.running == 0 && state.serving == 0)
            .await;
    }

    /// Admits a connection, unless the server is shutting down.
    fn admit(&self) -> Option<Admitted<PublicationOf<A>>> {
        // Subscribed before it is counted, so a connection the server counts
        // takes the closing publication.
        let (_, published) = self.publications.subscribe();
        let serving = Counted::take(&self.lifecycle, |state| &mut state.serving)?;
        Some(Admitted { published, serving })
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
                // Admitted before it is upgraded, so the shutdown waits for a
                // connection whose upgrade it did not refuse.
                let Some(admitted) = self.admit() else {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                };
                match authenticate(&parts) {
                    Some(principal) => upgrade
                        .on_upgrade(move |socket| self.serve_admitted(socket, principal, admitted))
                        .into_response(),
                    None => StatusCode::UNAUTHORIZED.into_response(),
                }
            }),
        )
    }

    /// Serves one WebSocket, authenticated as `principal`, until it closes or
    /// the server shuts down.
    pub async fn serve_socket(self: Arc<Self>, socket: WebSocket, principal: Principal) {
        match self.admit() {
            Some(admitted) => self.serve_admitted(socket, principal, admitted).await,
            None => {
                let mut socket = socket;
                let _ = socket.send(Message::Close(None)).await;
            }
        }
    }

    /// Serves one admitted WebSocket, authenticated as `principal`, until it
    /// closes or the server shuts down.
    async fn serve_admitted(
        self: Arc<Self>,
        socket: WebSocket,
        principal: Principal,
        admitted: Admitted<PublicationOf<A>>,
    ) {
        let Admitted {
            mut published,
            serving: _serving,
        } = admitted;
        let origin = self.connections.fetch_add(1, Ordering::Relaxed);
        let subscriptions: Subscriptions<A> =
            Mutex::new(ProjectionSubscriptions::new(principal.clone()));
        // The id each running command's response answers, by its ticket.
        let mut running: HashMap<u64, Option<Value>> = HashMap::new();
        let ran: RanCommands<A> = Arc::default();
        let mut tickets = 0;
        let (mut sink, mut stream) = socket.split();
        'serving: loop {
            let mut closing = false;
            let frames = tokio::select! {
                message = stream.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        match self.receive(origin, &subscriptions, text.as_str()).await {
                            Received::Answered(frames) => frames,
                            Received::Command { id, request } => {
                                tickets += 1;
                                let (ticket, ran) = (tickets, Arc::clone(&ran));
                                let answerable = move |sent: &Arc<Sent<PublicationOf<A>>>| {
                                    ran.lock()
                                        .unwrap_or_else(PoisonError::into_inner)
                                        .insert(ticket, Arc::clone(sent));
                                };
                                match self.start(origin, ticket, &principal, request, answerable) {
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
                    Ok(sent) if matches!(sent.publication, Publication::Closing) => {
                        closing = true;
                        Vec::new()
                    }
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
                        let closing_at = self.closing_at.load(Ordering::SeqCst);
                        closing = closing_at != 0 && closing_at <= skipped_through;
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
            if closing {
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
        }
    }

    /// Starts the command `request` names as `ticket` of `origin`, the
    /// connection that requested it or [`NO_CONNECTION`], as `principal`,
    /// unless the server
    /// is shutting down or is running as many commands as it allows. Once its
    /// host returns, its outcome is published, and handed to `ran` as it is
    /// numbered, before a later connection starts after it.
    fn start(
        self: &Arc<Self>,
        origin: u64,
        ticket: u64,
        principal: &Principal,
        request: ProjectionCommandRequest,
        ran: impl FnOnce(&Arc<Sent<PublicationOf<A>>>) + Send + 'static,
    ) -> Result<(), RpcFailure> {
        let Some(started) = Counted::take(&self.lifecycle, |state| &mut state.running) else {
            return Err(command_refusal(
                ProjectionErrorCode::Interrupted,
                "The server is shutting down.",
                &request.command,
            ));
        };
        let Ok(permit) = Arc::clone(&self.commands).try_acquire_owned() else {
            return Err(command_refusal(
                ProjectionErrorCode::RateLimit,
                "The server is running as many commands as it allows.",
                &request.command,
            ));
        };
        let server = Arc::clone(self);
        let principal = principal.clone();
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
            server.publications.send(publication, ran);
            // Published first, so the closing publication follows its outcome.
            drop(started);
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
            Publication::Ran { .. } | Publication::Closing => Vec::new(),
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
        let request_method = request.method.clone();
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
            Some(Ok(request)) => {
                let named = match &request {
                    ProjectionRequest::Subscribe(subscribe) => Some(subscribe.projection.clone()),
                    ProjectionRequest::Mutate(mutate) => Some(mutate.mutation.clone()),
                    _ => None,
                };
                AssertUnwindSafe(serve(&self.application, subscriptions, request))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        let operation = ProjectionOperation::for_method(&request_method)
                            .expect("a decoded request names a protocol method");
                        Err(refusal(
                            ProjectionErrorCode::InternalError,
                            "The request ended without a result.",
                            operation,
                            named.as_deref(),
                        ))
                    })
            }
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

/// `result`, as the protocol encodes the result of `operation` `name`.
fn encoded<R: Serialize>(
    result: &R,
    operation: ProjectionOperation,
    name: &str,
) -> Result<Value, RpcFailure> {
    serde_json::to_value(result).map_err(|error| {
        refusal(
            ProjectionErrorCode::InternalError,
            format!("The result of \"{name}\" could not be encoded: {error}"),
            operation,
            Some(name),
        )
    })
}

/// The refusal of the command `command`, addressed to it.
fn command_refusal(
    code: ProjectionErrorCode,
    message: impl Into<String>,
    command: &str,
) -> RpcFailure {
    refusal(code, message, ProjectionOperation::Command, Some(command))
}

/// The refusal of a request for `operation`, addressed to what it `named`.
fn refusal(
    code: ProjectionErrorCode,
    message: impl Into<String>,
    operation: ProjectionOperation,
    named: Option<&str>,
) -> RpcFailure {
    let message = message.into();
    RpcFailure::new(code, message.clone(), None).described(ProjectionErrorEnvelope::new(
        code, message, operation, named, None,
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
