//! Two clients editing one note through the notes server.

use std::sync::Arc;
use std::time::Duration;

use clerkenwell_axum::ProjectionServer;
use clerkenwell_doc::CollaborationReplica;
use clerkenwell_events::{ActorKind, Principal};
use clerkenwell_example_notes::server::{writer_named_in, NotesServer, PUBLICATION_WINDOW};
use clerkenwell_example_notes::NOTE;
use clerkenwell_store::CollaborationDocumentId;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A client of the server, numbering its requests and keeping a replica of
/// the note it subscribes to in step with every update it is sent.
struct Client {
    socket: Socket,
    requests: u64,
    held: Option<Held>,
}

/// The accepted note a client holds.
struct Held {
    replica: CollaborationReplica,
    frontier: String,
}

impl Client {
    /// A client connected as the writer `writer`.
    async fn connect(url: &str, writer: &str) -> Self {
        Self {
            socket: tokio_tungstenite::connect_async(format!("{url}?writer={writer}"))
                .await
                .expect("a connection")
                .0,
            requests: 0,
            held: None,
        }
    }

    /// The next frame, after taking the update it carries.
    async fn frame(&mut self) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(5), self.socket.next())
            .await
            .expect("a frame in time")
            .expect("an open connection")
            .expect("a frame");
        let frame: Value =
            serde_json::from_str(message.to_text().expect("a text frame")).expect("a JSON frame");
        let update = &frame["params"];
        match update["kind"].as_str() {
            Some("snapshot") => {
                let state = &update["snapshot"]["value"];
                self.held = Some(Held {
                    replica: replica(state),
                    frontier: frontier(state),
                });
            }
            Some("patch") => {
                let patch = &update["patch"]["value"];
                match patch["kind"].as_str() {
                    Some("replace") => {
                        let held = self.held.as_mut().expect("a patch follows a snapshot");
                        held.replica
                            .adopt_versioned_update_base64(
                                NOTE.schema_version,
                                patch["state"]["update_base64"].as_str().expect("an update"),
                            )
                            .expect("the accepted operations");
                        held.frontier = frontier(&patch["state"]);
                    }
                    _ => self.held = None,
                }
            }
            _ => {}
        }
        frame
    }

    /// Takes updates until the client holds the note at `frontier`, and
    /// returns the note.
    async fn holding(&mut self, frontier: &str) -> Value {
        loop {
            if let Some(held) = &self.held {
                if held.frontier == frontier {
                    return document(&held.replica);
                }
            }
            self.frame().await;
        }
    }

    /// The response to a request, taking the updates that arrive first.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.requests += 1;
        let id = self.requests;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.socket
            .send(Message::Text(request.to_string().into()))
            .await
            .expect("send");
        loop {
            let frame = self.frame().await;
            if frame["id"] == json!(id) {
                return frame;
            }
        }
    }

    async fn mutate(&mut self, mutation: &str, params: Value) -> Value {
        self.call(
            "projection.mutate",
            json!({ "mutation": mutation, "params": params }),
        )
        .await
    }

    /// Subscribes to a note's authoring state and returns the state its
    /// snapshot delivers.
    async fn subscribe(&mut self, note_id: &str) -> Value {
        let response = self
            .call(
                "projection.subscribe",
                json!({ "projection": "notes.authoringState", "params": { "note_id": note_id } }),
            )
            .await;
        let subscription_id = response["result"]["subscription_id"].clone();
        loop {
            let frame = self.frame().await;
            let update = &frame["params"];
            if update["subscription_id"] == subscription_id && update["kind"] == "snapshot" {
                return update["snapshot"]["value"].clone();
            }
        }
    }
}

/// The address of a new notes server, and the server.
async fn start() -> (String, Arc<ProjectionServer<NotesServer>>) {
    let server = ProjectionServer::new(NotesServer::default(), PUBLICATION_WINDOW);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let address = listener.local_addr().expect("an address");
    let router = server.clone().router("/projections", writer_named_in);
    tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
    (format!("ws://{address}/projections"), server)
}

/// A replica of the note an authoring state delivers.
fn replica(state: &Value) -> CollaborationReplica {
    CollaborationReplica::from_versioned_update_base64(
        NOTE,
        NOTE.schema_version,
        state["update_base64"].as_str().expect("an update"),
    )
    .expect("a replica")
}

fn frontier(state: &Value) -> String {
    state["accepted_frontier_base64"]
        .as_str()
        .expect("a frontier")
        .to_owned()
}

/// The note a replica holds.
fn document(replica: &CollaborationReplica) -> Value {
    replica.materialized_document("client").expect("a note")
}

/// Edits a replica of `note_id`'s `state` under the first peer of the block
/// the state allocated, and sends what it recorded.
async fn edit(
    client: &mut Client,
    note_id: &str,
    state: &Value,
    operation_id: &str,
    change: impl FnOnce(&mut Value),
) -> (CollaborationReplica, Value) {
    let mut replica = replica(state);
    let block = &state["peer_block"];
    replica
        .set_peer(
            block["base"]
                .as_str()
                .and_then(|base| base.parse().ok())
                .expect("a block's first peer"),
        )
        .expect("an allocated peer");
    let mut note = document(&replica);
    change(&mut note);
    replica.replace_document(&note).expect("an edit");
    let base = frontier(state);
    let response = client
        .mutate(
            "note.importUpdate",
            json!({
                "note_id": note_id,
                "operation_id": operation_id,
                "exchange_mode": "incremental",
                "base_frontier_base64": base,
                "update_base64": replica.export_incremental_update_base64(&base).expect("an update"),
                "peer_nonces": [block["nonce"]],
            }),
        )
        .await;
    (replica, response)
}

/// The frontier an accepted edit leaves the note at.
fn accepted(response: &Value) -> String {
    frontier(&response["result"]["result"]["value"])
}

async fn create(client: &mut Client, title: &str) -> String {
    let created = client
        .mutate("note.create", json!({ "title": title }))
        .await;
    created["result"]["result"]["value"]["note_id"]
        .as_str()
        .expect("a note id")
        .to_owned()
}

#[tokio::test]
async fn an_edit_one_client_sends_reaches_another_clients_subscription() {
    let (url, _) = start().await;
    let mut ada = Client::connect(&url, "ada").await;
    let mut grace = Client::connect(&url, "grace").await;
    let note_id = create(&mut ada, "Launch plan").await;
    ada.subscribe(&note_id).await;
    let state = grace.subscribe(&note_id).await;

    let (edited, response) = edit(&mut grace, &note_id, &state, "grace-body", |note| {
        note["body"] = json!("Ship on Friday.");
    })
    .await;
    assert!(response.get("error").is_none(), "{response}");

    let delivered = ada.holding(&accepted(&response)).await;
    assert_eq!(delivered["body"], document(&edited)["body"]);
}

#[tokio::test]
async fn a_refused_status_carries_what_its_writer_lacks_to_rebase() {
    let (url, _) = start().await;
    let mut ada = Client::connect(&url, "ada").await;
    let mut grace = Client::connect(&url, "grace").await;
    let note_id = create(&mut ada, "Launch plan").await;
    let ada_state = ada.subscribe(&note_id).await;
    let grace_state = grace.subscribe(&note_id).await;
    let (_, response) = edit(&mut ada, &note_id, &ada_state, "ada-status", |note| {
        note["status"] = json!("review");
    })
    .await;
    assert!(response.get("error").is_none(), "{response}");

    let (mut refused, response) =
        edit(&mut grace, &note_id, &grace_state, "grace-status", |note| {
            note["status"] = json!("published");
        })
        .await;

    let error = &response["error"];
    assert_eq!(
        error["data"]["projection_error"]["code"], "conflict",
        "{response}"
    );
    assert_eq!(error["data"]["projection_error"]["operation"], "mutate");
    refused
        .adopt_versioned_update_base64(
            NOTE.schema_version,
            error["data"]["missing_update_base64"]
                .as_str()
                .expect("what the writer lacks"),
        )
        .expect("take what the writer lacks");
    // The refused operations stay under the block they were written under.
    let rebased_state = json!({
        "accepted_frontier_base64": error["data"]["accepted_frontier_base64"],
        "update_base64": refused.export_update_base64().expect("the rebased replica"),
        "peer_block": grace_state["peer_block"],
    });
    let (rebased, response) = edit(
        &mut grace,
        &note_id,
        &rebased_state,
        "grace-status-rebased",
        |_| {},
    )
    .await;
    assert!(response.get("error").is_none(), "{response}");
    let delivered = grace.holding(&accepted(&response)).await;
    assert_eq!(delivered["status"], document(&rebased)["status"]);
}

#[tokio::test]
async fn a_note_a_writer_creates_is_attributed_to_that_writer() {
    let (url, server) = start().await;
    let mut ada = Client::connect(&url, "ada").await;

    let note_id = create(&mut ada, "Launch plan").await;

    let attribution = server
        .application()
        .service()
        .attribution(&CollaborationDocumentId::new(NOTE.name, &note_id))
        .expect("attribution")
        .expect("the note");
    let ada = Principal::new("ada", ActorKind::Human);
    assert!(!attribution.operations.is_empty() && !attribution.peers.is_empty());
    assert!(
        attribution
            .operations
            .iter()
            .all(|operation| operation.actor == ada),
        "{attribution:?}"
    );
    assert!(attribution
        .peers
        .values()
        .all(|principal| *principal == ada));
}

#[tokio::test]
async fn a_connection_naming_no_writer_is_refused() {
    let (url, _) = start().await;

    match tokio_tungstenite::connect_async(url.as_str()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 401)
        }
        Err(error) => panic!("expected an unauthorised response, got {error}"),
        Ok(_) => panic!("a connection naming no writer was served"),
    }
}
