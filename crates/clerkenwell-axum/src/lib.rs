//! A reference transport for the projection session protocol: JSON-RPC 2.0
//! over an axum WebSocket.
//!
//! Each connection keeps its own subscriptions. A mutation one connection
//! makes reaches the subscriptions of every other connection, and each change
//! a store announces reaches every connection's authoring-state
//! subscriptions. A connection that falls behind takes a snapshot for each of
//! its subscriptions.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::routing::get;
use axum::Router;
use clerkenwell_events::ChangeEvent;
use clerkenwell_session::transport::{
    ProjectionErrorCode, ProjectionErrorEnvelope, ProjectionTransportEvent,
    PROJECTION_UPDATE_NOTIFICATION,
};
use clerkenwell_session::{
    deliver_change, publish, resync_all, serve_request, AcceptedMutation, Held, ProjectionFailure,
    ProjectionHost, ProjectionRefusal, ProjectionSubscriptions,
};
use futures_util::{SinkExt, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

/// A refused command, as a JSON-RPC error answers it.
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
    /// An error the protocol layer raises before any command is served.
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

/// A mutation one connection accepted, for every other connection.
struct Publication<M> {
    origin: u64,
    mutation: AcceptedMutation<M>,
}

type Subscriptions<A> = Mutex<
    ProjectionSubscriptions<<A as ProjectionHost>::Patch, Held<<A as ProjectionHost>::Delivery>>,
>;

/// Serves an application's projections to every connection.
pub struct ProjectionServer<A: ProjectionHost> {
    application: A,
    published: broadcast::Sender<Arc<Publication<A::MutationResult>>>,
    changes: broadcast::Sender<Arc<ChangeEvent>>,
    connections: AtomicU64,
}

impl<A> ProjectionServer<A>
where
    A: ProjectionHost<Failure = RpcFailure> + Send + 'static,
    A::Snapshot: 'static,
    A::Patch: Sync + 'static,
    A::Delivery: Sync + 'static,
    A::MutationResult: 'static,
{
    /// A server whose connections resynchronise once they fall more than
    /// `publication_window` mutations, or changes to the application's
    /// stores, behind.
    pub fn new(application: A, publication_window: NonZeroUsize) -> Arc<Self> {
        let (published, _) = broadcast::channel(publication_window.get());
        let (changes, _) = broadcast::channel(publication_window.get());
        if let Some(authoring) = application.authoring_states() {
            let changes = changes.clone();
            authoring.stores().changes().listen(move |change| {
                // No connection is listening when the send fails.
                let _ = changes.send(Arc::new(change.clone()));
            });
        }
        Arc::new(Self {
            application,
            published,
            changes,
            connections: AtomicU64::new(1),
        })
    }

    pub fn application(&self) -> &A {
        &self.application
    }

    /// A router that serves the protocol to WebSocket connections at `path`.
    ///
    /// The route captures the server instead of taking it as axum `State`,
    /// which code scanning reads as request input and follows into the
    /// store's paths. The handler's only parameter is the client's upgrade.
    pub fn router(self: Arc<Self>, path: &str) -> Router {
        Router::new().route(
            path,
            get(move |upgrade: WebSocketUpgrade| async move {
                upgrade.on_upgrade(move |socket| self.serve_socket(socket))
            }),
        )
    }

    /// Serves one WebSocket until it closes.
    pub async fn serve_socket(self: Arc<Self>, socket: WebSocket) {
        let origin = self.connections.fetch_add(1, Ordering::Relaxed);
        let subscriptions: Subscriptions<A> = Mutex::new(ProjectionSubscriptions::default());
        let mut published = self.published.subscribe();
        let mut changes = self.changes.subscribe();
        let (mut sink, mut stream) = socket.split();
        loop {
            let frames = tokio::select! {
                message = stream.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        self.answer(origin, &subscriptions, text.as_str()).await
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                },
                publication = published.recv() => {
                    let events = match publication {
                        Ok(publication) if publication.origin == origin => continue,
                        Ok(publication) => {
                            publish(&self.application, &subscriptions, &publication.mutation).await.1
                        }
                        Err(RecvError::Lagged(missed)) => {
                            subscriptions
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .record_dropped_updates(missed);
                            resync_all(&self.application, &subscriptions, "broadcastLag").await
                        }
                        Err(RecvError::Closed) => break,
                    };
                    events.iter().filter_map(notification).collect()
                }
                change = changes.recv() => {
                    let events = match change {
                        Ok(change) => deliver_change(&self.application, &subscriptions, &change).await,
                        Err(RecvError::Lagged(missed)) => {
                            subscriptions
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .record_dropped_updates(missed);
                            resync_all(&self.application, &subscriptions, "broadcastLag").await
                        }
                        Err(RecvError::Closed) => break,
                    };
                    events.iter().filter_map(notification).collect()
                }
            };
            for frame in frames {
                if sink.send(Message::Text(frame.into())).await.is_err() {
                    return;
                }
            }
        }
    }

    /// The frames that answer one request: its response, when it has an id,
    /// then the updates it sends.
    async fn answer(
        &self,
        origin: u64,
        subscriptions: &Subscriptions<A>,
        text: &str,
    ) -> Vec<String> {
        let request = match serde_json::from_str::<Request>(text) {
            Ok(request) => request,
            Err(error) => {
                return vec![frame(
                    Value::Null,
                    Err::<Value, _>(ErrorObject::framing(PARSE_ERROR, error.to_string())),
                )]
            }
        };
        let id = request.id.clone();
        if request.jsonrpc != "2.0" {
            return vec![frame(
                id.unwrap_or(Value::Null),
                Err::<Value, _>(ErrorObject::framing(
                    INVALID_REQUEST,
                    "Only JSON-RPC 2.0 requests are served.",
                )),
            )];
        }
        let served = serve_request(
            &self.application,
            subscriptions,
            &request.method,
            request.params,
        )
        .await;
        let (response, events) = match served {
            None => (
                Err(ErrorObject::framing(
                    METHOD_NOT_FOUND,
                    format!("Unknown method \"{}\".", request.method),
                )),
                Vec::new(),
            ),
            Some(Err(failure)) => (Err(failure.error_object()), Vec::new()),
            Some(Ok(reply)) => {
                if let Some(mutation) = reply.accepted {
                    // No other connection is listening when the send fails.
                    let _ = self
                        .published
                        .send(Arc::new(Publication { origin, mutation }));
                }
                (Ok(reply.result), reply.events)
            }
        };
        let mut frames = Vec::new();
        if let Some(id) = id {
            frames.push(frame(id, response));
        }
        frames.extend(events.iter().filter_map(notification));
        frames
    }
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
