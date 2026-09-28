//! The projection protocol served over WebSocket connections.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::Duration;

use clerkenwell_axum::{ProjectionServer, RpcFailure, ServerWindows};
use clerkenwell_schema::{
    GeneratedMaterializationPlan, GeneratedMutationSpec, GeneratedProjectionSpec,
};
use clerkenwell_session::transport::ProjectionErrorCode;
use clerkenwell_session::{ProjectionHost, ProjectionRegistry, ProjectionSubscription};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

static PROJECTIONS: &[GeneratedProjectionSpec] = &[
    GeneratedProjectionSpec {
        name: "counters.value",
        depends_on: &["Counter"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
    GeneratedProjectionSpec {
        name: "clock.held",
        depends_on: &["Clock"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
];
static MUTATIONS: &[GeneratedMutationSpec] = &[GeneratedMutationSpec {
    name: "counter.increment",
    touches: &["Counter"],
}];
static REGISTRY: ProjectionRegistry = ProjectionRegistry::new(PROJECTIONS, MUTATIONS, &[]);

/// Counters by id, and a projection whose snapshot waits to be released.
struct Counters {
    values: Mutex<HashMap<String, u64>>,
    released: Semaphore,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            values: Mutex::default(),
            released: Semaphore::new(0),
        }
    }
}

fn counter_id(params: &Value) -> Result<String, RpcFailure> {
    params["counter_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| RpcFailure::new(ProjectionErrorCode::InvalidParams, "A counter id.", None))
}

impl ProjectionHost for Counters {
    type Snapshot = Value;
    type Patch = Value;
    type Delivery = ();
    type MutationResult = Value;
    type Failure = RpcFailure;

    fn registry(&self) -> &ProjectionRegistry {
        &REGISTRY
    }

    async fn snapshot(&self, projection: &str, params: &Value) -> Result<Value, RpcFailure> {
        if projection == "clock.held" {
            let _released = self.released.acquire().await.expect("the release");
            return Ok(json!({ "held": false }));
        }
        let id = counter_id(params)?;
        let value = self
            .values
            .lock()
            .expect("values")
            .get(&id)
            .copied()
            .unwrap_or(0);
        Ok(json!({ "value": value }))
    }

    fn delivered(&self, _snapshot: &Value) -> Option<()> {
        None
    }

    async fn revision_floor(&self, _projection: &str, _params: &Value) -> Result<u64, RpcFailure> {
        Ok(0)
    }

    async fn mutation_revision_floor(&self) -> Result<u64, RpcFailure> {
        Ok(0)
    }

    async fn mutate(&self, _mutation: &str, params: Value) -> Result<Value, RpcFailure> {
        let id = counter_id(&params)?;
        let mut values = self.values.lock().expect("values");
        let value = values.entry(id.clone()).or_default();
        *value += 1;
        Ok(json!({ "counter_id": id, "value": *value }))
    }

    async fn mutation_patch(
        &self,
        _mutation: &str,
        result: &Value,
        subscription: &ProjectionSubscription<()>,
    ) -> Option<Value> {
        (subscription.params["counter_id"] == result["counter_id"])
            .then(|| json!({ "value": result["value"] }))
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start(window: usize) -> (String, std::sync::Arc<ProjectionServer<Counters>>) {
    let windows = ServerWindows {
        publications: NonZeroUsize::new(window).expect("a publication window"),
        retained_patches: 64,
    };
    let server = ProjectionServer::new(Counters::default(), windows);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let address = listener.local_addr().expect("an address");
    let router = server.clone().router("/projections");
    tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
    (format!("ws://{address}/projections"), server)
}

async fn connect(url: &str) -> Socket {
    tokio_tungstenite::connect_async(url)
        .await
        .expect("a connection")
        .0
}

async fn send(socket: &mut Socket, frame: Value) {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .expect("send");
}

async fn request(socket: &mut Socket, id: u64, method: &str, params: Value) {
    send(
        socket,
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )
    .await;
}

async fn next(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("a frame in time")
        .expect("an open connection")
        .expect("a frame");
    serde_json::from_str(message.to_text().expect("a text frame")).expect("a JSON frame")
}

/// Whether nothing arrives for a moment.
async fn quiet(socket: &mut Socket) -> bool {
    tokio::time::timeout(Duration::from_millis(200), socket.next())
        .await
        .is_err()
}

async fn subscribe(socket: &mut Socket, id: u64, projection: &str, params: Value) -> Value {
    request(
        socket,
        id,
        "projection.subscribe",
        json!({ "projection": projection, "params": params }),
    )
    .await;
    let response = next(socket).await;
    assert_eq!(response["id"], json!(id), "{response}");
    response["result"].clone()
}

async fn increment(socket: &mut Socket, id: u64, counter: &str) -> Value {
    request(
        socket,
        id,
        "projection.mutate",
        json!({ "mutation": "counter.increment", "params": { "counter_id": counter } }),
    )
    .await;
    let response = next(socket).await;
    assert_eq!(response["id"], json!(id), "{response}");
    response["result"].clone()
}

fn update(frame: &Value) -> &Value {
    assert_eq!(frame["method"], "projection.update", "{frame}");
    &frame["params"]
}

#[tokio::test]
async fn a_subscriber_receives_its_reply_then_its_snapshot() {
    let (url, _) = start(8).await;
    let mut socket = connect(&url).await;

    let accepted = subscribe(
        &mut socket,
        1,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;

    let snapshot = next(&mut socket).await;
    let snapshot = update(&snapshot);
    assert_eq!(snapshot["kind"], "snapshot");
    assert_eq!(snapshot["subscription_id"], accepted["subscription_id"]);
    assert_eq!(snapshot["revision"], accepted["revision"]);
    assert_eq!(snapshot["snapshot"], json!({ "value": 0 }));
}

#[tokio::test]
async fn a_mutation_on_one_connection_patches_a_subscription_on_another() {
    let (url, _) = start(8).await;
    let mut reader = connect(&url).await;
    let mut writer = connect(&url).await;
    let accepted = subscribe(
        &mut reader,
        1,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;
    next(&mut reader).await;

    increment(&mut writer, 1, "a").await;

    let patch = next(&mut reader).await;
    let patch = update(&patch);
    assert_eq!(patch["kind"], "patch");
    assert_eq!(patch["subscription_id"], accepted["subscription_id"]);
    assert_eq!(patch["from_revision"], accepted["revision"]);
    assert_eq!(patch["patch"], json!({ "value": 1 }));
}

#[tokio::test]
async fn a_mutation_patches_its_own_connection_once() {
    let (url, _) = start(8).await;
    let mut socket = connect(&url).await;
    subscribe(
        &mut socket,
        1,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;
    next(&mut socket).await;

    let accepted = increment(&mut socket, 2, "a").await;

    let patch = next(&mut socket).await;
    assert_eq!(update(&patch)["to_revision"], accepted["revision"]);
    assert!(quiet(&mut socket).await, "a second patch arrived");
}

#[tokio::test]
async fn a_mutation_patches_no_subscription_it_does_not_affect() {
    let (url, _) = start(8).await;
    let mut reader = connect(&url).await;
    let mut writer = connect(&url).await;
    subscribe(
        &mut reader,
        1,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;
    next(&mut reader).await;

    increment(&mut writer, 1, "b").await;

    assert!(
        quiet(&mut reader).await,
        "a patch arrived for another counter"
    );
}

#[tokio::test]
async fn a_connection_that_falls_behind_takes_a_snapshot_of_each_subscription() {
    let (url, server) = start(1).await;
    let mut reader = connect(&url).await;
    let mut writer = connect(&url).await;
    let counter = subscribe(
        &mut reader,
        1,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;
    next(&mut reader).await;
    // The reader's connection waits on this snapshot while the writer mutates.
    request(
        &mut reader,
        2,
        "projection.subscribe",
        json!({ "projection": "clock.held" }),
    )
    .await;
    for id in 1..=3 {
        increment(&mut writer, id, "a").await;
    }
    server.application().released.add_permits(8);

    // The held subscription answers, then every subscription is resent.
    let resynced = loop {
        let frame = next(&mut reader).await;
        let params = &frame["params"];
        if frame["method"] == "projection.update"
            && params["kind"] == "snapshot"
            && params["subscription_id"] == counter["subscription_id"]
        {
            break params.clone();
        }
    };
    assert_eq!(resynced["snapshot"], json!({ "value": 3 }));
    assert!(resynced["revision"].as_u64() > counter["revision"].as_u64());
}

#[tokio::test]
async fn a_refused_command_answers_with_its_error_and_envelope() {
    let (url, _) = start(8).await;
    let mut socket = connect(&url).await;

    request(
        &mut socket,
        1,
        "projection.mutate",
        json!({ "mutation": "counter.explode", "params": {} }),
    )
    .await;

    let response = next(&mut socket).await;
    assert_eq!(response["id"], json!(1));
    assert_eq!(response["error"]["code"], json!(-32602));
    let data = &response["error"]["data"];
    assert_eq!(data["code"], "unknown_mutation");
    assert_eq!(data["projection_error"]["code"], "unknown_mutation");
    assert_eq!(data["projection_error"]["operation"], "mutate");
    assert_eq!(data["projection_error"]["name"], "counter.explode");
}

#[tokio::test]
async fn a_request_outside_the_protocol_or_not_json_rpc_is_refused() {
    let (url, _) = start(8).await;
    let mut socket = connect(&url).await;

    request(&mut socket, 1, "session.list", json!({})).await;
    assert_eq!(next(&mut socket).await["error"]["code"], json!(-32601));

    socket
        .send(Message::Text("not json".into()))
        .await
        .expect("send");
    assert_eq!(next(&mut socket).await["error"]["code"], json!(-32700));

    send(
        &mut socket,
        json!({ "jsonrpc": "1.0", "id": 2, "method": "projection.subscribe" }),
    )
    .await;
    let response = next(&mut socket).await;
    assert_eq!(response["id"], json!(2));
    assert_eq!(response["error"]["code"], json!(-32600));
}
