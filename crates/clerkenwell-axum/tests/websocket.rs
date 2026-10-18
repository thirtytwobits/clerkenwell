//! The projection protocol served over WebSocket connections.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clerkenwell_axum::{ProjectionServer, RpcFailure};
use clerkenwell_events::{ActorKind, Principal};
use clerkenwell_schema::{
    GeneratedCommandSpec, GeneratedMaterializationPlan, GeneratedMutationSpec,
    GeneratedProjectionSpec,
};
use clerkenwell_session::transport::ProjectionErrorCode;
use clerkenwell_session::{
    AnnouncementFeed, Held, ModificationCounter, Modified, NumberedSnapshot, ProjectionHost,
    ProjectionRegistry, ProjectionSubscription,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
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
static COMMANDS: &[GeneratedCommandSpec] = &[
    GeneratedCommandSpec {
        name: "counter.reset",
        touches: &["Counter"],
    },
    GeneratedCommandSpec {
        name: "counter.countTo",
        touches: &["Counter"],
    },
];
static REGISTRY: ProjectionRegistry =
    ProjectionRegistry::new(PROJECTIONS, MUTATIONS, COMMANDS, &[]);

/// Counters by id, a projection whose snapshot waits to be released, gates a
/// command waits at until the test opens them, and the feed the counters
/// announce each value a count reaches on.
struct Counters {
    values: Mutex<HashMap<String, u64>>,
    released: Semaphore,
    gates: Mutex<HashMap<String, Arc<Semaphore>>>,
    announcements: AnnouncementFeed<Value>,
    /// Numbered while the values' lock is held, as each value is saved.
    modifications: ModificationCounter,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            values: Mutex::default(),
            released: Semaphore::new(0),
            gates: Mutex::default(),
            announcements: AnnouncementFeed::default(),
            modifications: ModificationCounter::default(),
        }
    }
}

impl Counters {
    /// The gate `name`, closed until it is opened.
    fn gate(&self, name: &str) -> Arc<Semaphore> {
        Arc::clone(
            self.gates
                .lock()
                .expect("gates")
                .entry(name.to_owned())
                .or_insert_with(|| Arc::new(Semaphore::new(0))),
        )
    }

    fn open(&self, name: &str) {
        self.gate(name).add_permits(1);
    }

    /// Saves `value` as counter `id`, and returns the modification's number.
    fn set(&self, id: &str, value: u64) -> u64 {
        let mut values = self.values.lock().expect("values");
        let modification = self.modifications.take();
        values.insert(id.to_owned(), value);
        modification
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
    type CommandResult = Value;
    type Announcement = Value;
    type Failure = RpcFailure;

    fn registry(&self) -> &ProjectionRegistry {
        &REGISTRY
    }

    fn announcements(&self) -> Option<&AnnouncementFeed<Value>> {
        Some(&self.announcements)
    }

    fn modifications(&self) -> &ModificationCounter {
        &self.modifications
    }

    async fn snapshot(
        &self,
        projection: &str,
        params: &Value,
        _held: Option<&()>,
    ) -> Result<NumberedSnapshot<Value>, RpcFailure> {
        if projection == "clock.held" {
            let _released = self.released.acquire().await.expect("the release");
            return Ok(NumberedSnapshot {
                snapshot: json!({ "held": false }),
                through: self.modifications.last(),
            });
        }
        let id = counter_id(params)?;
        let values = self.values.lock().expect("values");
        Ok(NumberedSnapshot {
            snapshot: json!({ "value": values.get(&id).copied().unwrap_or(0) }),
            through: self.modifications.last(),
        })
    }

    fn delivered(&self, _snapshot: &Value) -> Option<()> {
        None
    }

    fn delivered_by_patch(&self, _patch: &Value) -> Option<()> {
        None
    }

    async fn mutate(
        &self,
        principal: &Principal,
        _mutation: &str,
        params: Value,
    ) -> Result<Modified<Value>, RpcFailure> {
        if let Some(details) = params.get("refuse") {
            return Err(RpcFailure::new(
                ProjectionErrorCode::Conflict,
                "Refused.",
                Some(details.clone()),
            ));
        }
        let id = counter_id(&params)?;
        let mut values = self.values.lock().expect("values");
        let modification = self.modifications.take();
        let value = values.entry(id.clone()).or_default();
        *value += 1;
        Ok(Modified {
            result: json!({ "counter_id": id, "value": *value, "by": principal.id }),
            modification: Some(modification),
        })
    }

    async fn mutation_patch(
        &self,
        _mutation: &str,
        result: &Value,
        subscription: &ProjectionSubscription<Held<()>>,
    ) -> Option<Value> {
        (subscription.params["counter_id"] == result["counter_id"])
            .then(|| json!({ "value": result["value"] }))
    }

    async fn command(
        &self,
        principal: &Principal,
        command: &str,
        params: Value,
    ) -> Result<Modified<Value>, RpcFailure> {
        if let Some(gate) = params["gate"].as_str() {
            let _opened = self.gate(gate).acquire_owned().await.expect("the gate");
        }
        assert!(params.get("fail").is_none(), "the command failed");
        let id = counter_id(&params)?;
        if command == "counter.countTo" {
            let to = params["to"].as_u64().expect("a count");
            for value in 1..=to {
                let modification = self.set(&id, value);
                self.announcements.announce(
                    &["Counter"],
                    modification,
                    json!({ "counter_id": id, "value": value }),
                );
            }
            // Each value the count reaches was announced.
            return Ok(Modified {
                result: json!({ "counter_id": id, "value": to, "by": principal.id }),
                modification: None,
            });
        }
        let modification = self.set(&id, 0);
        if let Some(saved) = params["saved"].as_str() {
            self.open(saved);
        }
        if let Some(gate) = params["then_gate"].as_str() {
            let _opened = self.gate(gate).acquire_owned().await.expect("the gate");
        }
        Ok(Modified {
            result: json!({ "counter_id": id, "value": 0, "by": principal.id }),
            modification: Some(modification),
        })
    }

    async fn command_patch(
        &self,
        _command: &str,
        result: &Value,
        subscription: &ProjectionSubscription<Held<()>>,
    ) -> Option<Value> {
        (subscription.params["counter_id"] == result["counter_id"])
            .then(|| json!({ "value": result["value"] }))
    }

    async fn announcement_patch(
        &self,
        announcement: &Value,
        subscription: &ProjectionSubscription<Held<()>>,
    ) -> Option<Value> {
        (subscription.params["counter_id"] == announcement["counter_id"])
            .then(|| json!({ "value": announcement["value"] }))
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start(window: usize) -> (String, std::sync::Arc<ProjectionServer<Counters>>) {
    start_with(window, 8).await
}

/// A server that runs at most `commands` commands at once.
async fn start_with(
    window: usize,
    commands: usize,
) -> (String, std::sync::Arc<ProjectionServer<Counters>>) {
    let window = NonZeroUsize::new(window).expect("a publication window");
    let commands = NonZeroUsize::new(commands).expect("a command limit");
    let server = ProjectionServer::new(Counters::default(), window, commands);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let address = listener.local_addr().expect("an address");
    let router = server.clone().router("/projections", |parts| {
        let id = parts
            .headers
            .get("authorization")?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        Some(Principal::new(id, ActorKind::Human))
    });
    tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
    (format!("ws://{address}/projections"), server)
}

/// The upgrade request to `url` of a client presenting `id`'s credentials.
fn upgrade_as(url: &str, id: &str) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = url.into_client_request().expect("a request");
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {id}").parse().expect("a header value"),
    );
    request
}

async fn connect_as(url: &str, id: &str) -> Socket {
    tokio_tungstenite::connect_async(upgrade_as(url, id))
        .await
        .expect("a connection")
        .0
}

async fn connect(url: &str) -> Socket {
    connect_as(url, "client").await
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

async fn reset(socket: &mut Socket, id: u64, counter: &str) -> Value {
    request(
        socket,
        id,
        "projection.command",
        json!({
            "command": "counter.reset",
            "operation_id": format!("reset-{id}"),
            "params": { "counter_id": counter },
        }),
    )
    .await;
    let response = next(socket).await;
    assert_eq!(response["id"], json!(id), "{response}");
    response["result"].clone()
}

async fn reset_at_gate(socket: &mut Socket, id: u64, counter: &str, gate: &str) {
    request(
        socket,
        id,
        "projection.command",
        json!({
            "command": "counter.reset",
            "params": { "counter_id": counter, "gate": gate },
        }),
    )
    .await;
}

fn update(frame: &Value) -> &Value {
    assert_eq!(frame["method"], "projection.update", "{frame}");
    &frame["params"]
}

#[tokio::test]
async fn an_upgrade_the_application_authenticates_as_no_principal_is_refused() {
    let (url, _) = start(8).await;

    match tokio_tungstenite::connect_async(url.as_str()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 401)
        }
        Err(error) => panic!("expected an unauthorised response, got {error}"),
        Ok(_) => panic!("an unauthenticated upgrade was served"),
    }
}

#[tokio::test]
async fn each_request_is_made_as_the_principal_its_connection_authenticated_as() {
    let (url, _) = start(8).await;
    let mut alice = connect_as(&url, "alice").await;
    let mut bob = connect_as(&url, "bob").await;

    assert_eq!(increment(&mut alice, 1, "a").await["result"]["by"], "alice");
    assert_eq!(increment(&mut bob, 1, "a").await["result"]["by"], "bob");
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
async fn a_command_answers_its_caller_then_patches_its_own_connection() {
    let (url, _) = start(8).await;
    let mut socket = connect_as(&url, "alice").await;
    subscribe(
        &mut socket,
        1,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;
    next(&mut socket).await;
    increment(&mut socket, 2, "a").await;
    next(&mut socket).await;

    let accepted = reset(&mut socket, 3, "a").await;

    assert_eq!(accepted["operation_id"], "reset-3");
    assert_eq!(accepted["result"]["by"], "alice");
    let patch = next(&mut socket).await;
    let patch = update(&patch);
    assert_eq!(patch["to_revision"], accepted["revision"]);
    assert_eq!(patch["patch"]["value"], accepted["result"]["value"]);
}

#[tokio::test]
async fn a_command_on_one_connection_patches_a_subscription_on_another() {
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
    let incremented = next(&mut reader).await;
    let incremented = update(&incremented);

    let reset = reset(&mut writer, 2, "a").await;

    let patch = next(&mut reader).await;
    let patch = update(&patch);
    assert_eq!(patch["kind"], "patch");
    assert_eq!(patch["subscription_id"], accepted["subscription_id"]);
    assert_eq!(patch["from_revision"], incremented["to_revision"]);
    assert_eq!(patch["patch"]["value"], reset["result"]["value"]);
}

#[tokio::test]
async fn a_running_command_holds_back_none_of_its_connections_requests_or_updates() {
    let (url, server) = start(8).await;
    let mut socket = connect(&url).await;
    let mut other = connect(&url).await;
    subscribe(
        &mut socket,
        1,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;
    next(&mut socket).await;
    reset_at_gate(&mut socket, 2, "a", "reset").await;

    let incremented = increment(&mut socket, 3, "a").await;
    let own = next(&mut socket).await;
    assert_eq!(update(&own)["to_revision"], incremented["revision"]);
    increment(&mut other, 1, "a").await;
    let theirs = next(&mut socket).await;
    let theirs = update(&theirs);
    server.application().open("reset");

    let response = next(&mut socket).await;
    assert_eq!(response["id"], json!(2), "{response}");
    let patch = next(&mut socket).await;
    let patch = update(&patch);
    assert_eq!(patch["from_revision"], theirs["to_revision"]);
    assert_eq!(patch["to_revision"], response["result"]["revision"]);
    assert_eq!(
        patch["patch"]["value"],
        response["result"]["result"]["value"]
    );
}

#[tokio::test]
async fn commands_are_answered_by_their_ids_in_the_order_they_finish() {
    let (url, server) = start(8).await;
    let mut socket = connect(&url).await;
    reset_at_gate(&mut socket, 1, "x", "first").await;
    reset_at_gate(&mut socket, 2, "y", "second").await;

    server.application().open("second");
    let second = next(&mut socket).await;
    server.application().open("first");
    let first = next(&mut socket).await;

    assert_eq!(second["id"], json!(2), "{second}");
    assert_eq!(second["result"]["result"]["counter_id"], "y");
    assert_eq!(first["id"], json!(1), "{first}");
    assert_eq!(first["result"]["result"]["counter_id"], "x");
}

#[tokio::test]
async fn a_command_running_when_its_connection_closes_still_reaches_other_connections() {
    let (url, server) = start(8).await;
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
    increment(&mut writer, 1, "a").await;
    let incremented = next(&mut reader).await;
    let incremented = update(&incremented);

    reset_at_gate(&mut writer, 2, "a", "reset").await;
    writer.close(None).await.expect("close");
    server.application().open("reset");

    let patch = next(&mut reader).await;
    let patch = update(&patch);
    assert_eq!(patch["from_revision"], incremented["to_revision"]);
    assert_ne!(patch["patch"]["value"], incremented["patch"]["value"]);
}

#[tokio::test]
async fn a_command_whose_run_ends_without_a_result_is_refused_addressed_to_it() {
    let (url, _) = start(8).await;
    let mut socket = connect(&url).await;

    request(
        &mut socket,
        1,
        "projection.command",
        json!({ "command": "counter.reset", "params": { "counter_id": "a", "fail": true } }),
    )
    .await;

    let response = next(&mut socket).await;
    assert_eq!(response["id"], json!(1));
    let data = &response["error"]["data"];
    assert_eq!(data["code"], "internal_error");
    assert_eq!(data["projection_error"]["operation"], "command");
    assert_eq!(data["projection_error"]["name"], "counter.reset");
}

#[tokio::test]
async fn a_command_s_announcements_reach_its_caller_before_its_response() {
    let (url, _) = start(8).await;
    let mut socket = connect(&url).await;
    let accepted = subscribe(
        &mut socket,
        1,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;
    next(&mut socket).await;
    let count = 3;

    request(
        &mut socket,
        2,
        "projection.command",
        json!({ "command": "counter.countTo", "params": { "counter_id": "a", "to": count } }),
    )
    .await;

    let mut held = accepted["revision"].clone();
    for value in 1..=count {
        let patch = next(&mut socket).await;
        let patch = update(&patch);
        assert_eq!(patch["subscription_id"], accepted["subscription_id"]);
        assert_eq!(patch["from_revision"], held);
        assert_eq!(patch["patch"]["value"], json!(value));
        held = patch["to_revision"].clone();
    }
    let response = next(&mut socket).await;
    assert_eq!(response["id"], json!(2), "{response}");
    assert_eq!(response["result"]["result"]["value"], json!(count));
}

#[tokio::test]
async fn an_announcement_reaches_every_connection_subscribed_to_what_it_changed_and_no_other() {
    let (url, server) = start(8).await;
    let mut first = connect(&url).await;
    let mut second = connect(&url).await;
    let mut elsewhere = connect(&url).await;
    for socket in [&mut first, &mut second] {
        subscribe(socket, 1, "counters.value", json!({ "counter_id": "a" })).await;
        next(socket).await;
    }
    subscribe(
        &mut elsewhere,
        1,
        "counters.value",
        json!({ "counter_id": "b" }),
    )
    .await;
    next(&mut elsewhere).await;

    let counters = server.application();
    for (id, value) in [("a", 7), ("b", 9)] {
        counters.announcements.announce(
            &["Counter"],
            counters.set(id, value),
            json!({ "counter_id": id, "value": value }),
        );
    }

    for socket in [&mut first, &mut second] {
        let patch = next(socket).await;
        assert_eq!(update(&patch)["patch"], json!({ "value": 7 }));
    }
    let patch = next(&mut elsewhere).await;
    assert_eq!(
        update(&patch)["patch"],
        json!({ "value": 9 }),
        "the first update another counter's subscriber takes is its own counter's"
    );
}

#[tokio::test]
async fn a_command_beyond_the_servers_limit_is_refused_as_rate_limit_until_one_finishes() {
    let (url, server) = start_with(8, 1).await;
    let mut first = connect(&url).await;
    let mut second = connect(&url).await;
    reset_at_gate(&mut first, 1, "a", "held").await;
    // The first connection serves its frames in order, so its command has
    // started once this answers.
    increment(&mut first, 2, "b").await;

    request(
        &mut second,
        1,
        "projection.command",
        json!({ "command": "counter.reset", "params": { "counter_id": "c" } }),
    )
    .await;
    let refused = next(&mut second).await;
    assert_eq!(refused["id"], json!(1), "{refused}");
    let envelope = &refused["error"]["data"]["projection_error"];
    assert_eq!(envelope["code"], "rate_limit");
    assert_eq!(envelope["retryable"], true);
    assert_eq!(envelope["operation"], "command");
    assert_eq!(envelope["name"], "counter.reset");

    server.application().open("held");
    assert_eq!(next(&mut first).await["id"], json!(1));
    let retried = reset(&mut second, 2, "c").await;
    assert_eq!(retried["result"]["counter_id"], "c");
}

#[tokio::test]
async fn a_connection_that_falls_behind_skips_every_publication_its_snapshots_hold() {
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

    increment(&mut writer, 4, "a").await;

    let patch = loop {
        let frame = next(&mut reader).await;
        if frame["method"] == "projection.update"
            && frame["params"]["subscription_id"] == counter["subscription_id"]
        {
            break frame["params"].clone();
        }
    };
    assert_eq!(patch["kind"], "patch");
    assert_eq!(patch["from_revision"], resynced["revision"]);
    assert_ne!(patch["patch"]["value"], resynced["snapshot"]["value"]);
}

#[tokio::test]
async fn a_subscription_whose_snapshot_holds_a_running_command_s_modification_is_not_sent_it() {
    let (url, server) = start(8).await;
    let mut socket = connect(&url).await;
    increment(&mut socket, 1, "a").await;
    request(
        &mut socket,
        2,
        "projection.command",
        json!({
            "command": "counter.reset",
            "params": { "counter_id": "a", "saved": "reset saved", "then_gate": "reset" },
        }),
    )
    .await;
    let saved = server.application().gate("reset saved");
    let _saved = saved.acquire().await.expect("the reset is saved");

    let accepted = subscribe(
        &mut socket,
        3,
        "counters.value",
        json!({ "counter_id": "a" }),
    )
    .await;
    let snapshot = next(&mut socket).await;
    server.application().open("reset");
    let response = next(&mut socket).await;
    assert_eq!(response["id"], json!(2), "{response}");
    assert_eq!(
        update(&snapshot)["snapshot"]["value"],
        response["result"]["result"]["value"],
        "the snapshot holds the reset"
    );

    // Nothing is sent between the command's response and the next request's.
    let incremented = increment(&mut socket, 4, "a").await;
    let patch = next(&mut socket).await;
    let patch = update(&patch);
    assert_eq!(patch["from_revision"], accepted["revision"]);
    assert_eq!(patch["patch"]["value"], incremented["result"]["value"]);
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
async fn a_refused_request_answers_with_its_error_and_envelope() {
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
async fn an_unknown_command_answers_with_its_error_and_envelope() {
    let (url, _) = start(8).await;
    let mut socket = connect(&url).await;

    for (id, unknown) in [(1, "counter.explode"), (2, "counter.increment")] {
        request(
            &mut socket,
            id,
            "projection.command",
            json!({ "command": unknown, "params": {} }),
        )
        .await;

        let response = next(&mut socket).await;
        assert_eq!(response["id"], json!(id));
        assert_eq!(response["error"]["code"], json!(-32602));
        let data = &response["error"]["data"];
        assert_eq!(data["code"], "unknown_command");
        assert_eq!(data["projection_error"]["operation"], "command");
        assert_eq!(data["projection_error"]["name"], unknown);
    }
}

#[tokio::test]
async fn a_refusal_s_details_join_its_error_data_beneath_its_protocol_code() {
    let (url, _) = start(8).await;
    let mut socket = connect(&url).await;

    for (id, details) in [
        (1, json!({ "reason": "taken", "code": "spoofed" })),
        (2, json!(7)),
    ] {
        request(
            &mut socket,
            id,
            "projection.mutate",
            json!({ "mutation": "counter.increment", "params": { "refuse": details } }),
        )
        .await;
    }

    let object = next(&mut socket).await;
    let data = &object["error"]["data"];
    assert_eq!(object["id"], json!(1));
    assert_eq!(data["reason"], "taken");
    assert_eq!(data["code"], "conflict");
    assert_eq!(data["projection_error"]["code"], "conflict");

    let other = next(&mut socket).await;
    let data = &other["error"]["data"];
    assert_eq!(other["id"], json!(2));
    assert_eq!(data["payload"], json!(7));
    assert_eq!(data["code"], "conflict");
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
