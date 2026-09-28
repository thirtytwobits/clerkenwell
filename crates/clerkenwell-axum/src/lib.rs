//! A reference transport for the projection session protocol: JSON-RPC 2.0
//! over an axum WebSocket.
//!
//! Each connection keeps its own subscriptions. A mutation one connection
//! makes reaches the subscriptions of every other connection, and a
//! connection that falls behind the mutations of others takes a snapshot for
//! each of its subscriptions.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::routing::get;
use axum::Router;
use clerkenwell_session::transport::{
    ProjectionErrorCode, ProjectionErrorEnvelope, ProjectionTransportEvent,
    PROJECTION_UPDATE_NOTIFICATION,
};
use clerkenwell_session::{
    publish, resync_all, serve_request, AcceptedMutation, ProjectionFailure, ProjectionHost,
    ProjectionRefusal, ProjectionSubscriptions,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

/// How many mutations a connection may fall behind before it resynchronises,
/// unless the application chooses otherwise.
pub const DEFAULT_PUBLICATION_WINDOW: usize = 256;

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
    fn error_object(&self) -> Value {
        let mut data = match &self.refusal.details {
            Some(Value::Object(details)) => details.clone(),
            Some(other) => serde_json::Map::from_iter([("payload".to_owned(), other.clone())]),
            None => serde_json::Map::new(),
        };
        data.insert("code".to_owned(), json!(self.refusal.code));
        if let Some(envelope) = &self.envelope {
            data.insert("projection_error".to_owned(), json!(envelope));
        }
        json!({
            "code": rpc_code(self.refusal.code),
            "message": self.refusal.message,
            "data": data,
        })
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
        | ProjectionErrorCode::ValidationFailed
        | ProjectionErrorCode::CursorAhead => -32602,
        ProjectionErrorCode::NotFound => -32004,
        ProjectionErrorCode::Conflict
        | ProjectionErrorCode::StaleWrite
        | ProjectionErrorCode::BaseRevisionInFuture => -32009,
        ProjectionErrorCode::RateLimit | ProjectionErrorCode::InternalError => -32000,
    }
}

#[derive(Debug, Deserialize)]
struct Request {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct Notification<'a, T> {
    jsonrpc: &'static str,
    method: &'static str,
    params: &'a T,
}

/// A mutation one connection accepted, for every other connection.
struct Publication<M> {
    origin: u64,
    mutation: AcceptedMutation<M>,
}

type Subscriptions<A> =
    Mutex<ProjectionSubscriptions<<A as ProjectionHost>::Patch, <A as ProjectionHost>::Delivery>>;

/// Serves an application's projections to every connection.
pub struct ProjectionServer<A: ProjectionHost> {
    application: A,
    published: broadcast::Sender<Arc<Publication<A::MutationResult>>>,
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
    pub fn new(application: A) -> Arc<Self> {
        Self::with_publication_window(application, DEFAULT_PUBLICATION_WINDOW)
    }

    /// A server whose connections resynchronise once they fall more than
    /// `window` mutations behind the others.
    pub fn with_publication_window(application: A, window: usize) -> Arc<Self> {
        let (published, _) = broadcast::channel(window);
        Arc::new(Self {
            application,
            published,
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
                return vec![error_frame(
                    Value::Null,
                    json!({ "code": PARSE_ERROR, "message": error.to_string() }),
                )]
            }
        };
        let id = request.id.clone();
        if request.jsonrpc != "2.0" {
            return vec![error_frame(
                id.unwrap_or(Value::Null),
                json!({ "code": INVALID_REQUEST, "message": "Only JSON-RPC 2.0 requests are served." }),
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
                Err(json!({
                    "code": METHOD_NOT_FOUND,
                    "message": format!("Unknown method \"{}\".", request.method),
                })),
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
            frames.push(match response {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
                Err(error) => error_frame(id, error),
            });
        }
        frames.extend(events.iter().filter_map(notification));
        frames
    }
}

fn error_frame(id: Value, error: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": error }).to_string()
}

/// An update as the notification that sends it. An update that cannot be
/// encoded is not sent: the subscription's next update then does not follow
/// what its client holds, and the client resynchronises.
fn notification<S: Serialize, P: Serialize>(
    event: &ProjectionTransportEvent<S, P>,
) -> Option<String> {
    serde_json::to_string(&Notification {
        jsonrpc: "2.0",
        method: PROJECTION_UPDATE_NOTIFICATION,
        params: event,
    })
    .ok()
}
