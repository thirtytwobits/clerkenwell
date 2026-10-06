//! TypeScript clients sync a note through the Rust notes server.

use std::sync::Arc;

use clerkenwell_axum::ProjectionServer;
use clerkenwell_conformance::workspace;
use clerkenwell_example_notes::server::{writer_named_in, NotesServer, PUBLICATION_WINDOW};
use clerkenwell_example_notes::NOTE;
use clerkenwell_store::CollaborationDocumentId;
use serde_json::Value;

/// A fresh notes server's address, and the server.
async fn serve() -> (String, Arc<ProjectionServer<NotesServer>>) {
    let server = ProjectionServer::new(NotesServer::default(), PUBLICATION_WINDOW);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let url = format!(
        "ws://{}/projections",
        listener.local_addr().expect("an address")
    );
    let router = server.clone().router("/projections", writer_named_in);
    tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
    (url, server)
}

/// Serves notes on a fresh server and runs the TypeScript clients against it
/// with `arguments`, returning each line they print as JSON.
async fn run_clients(arguments: &[&str]) -> Vec<Value> {
    let (url, _) = serve().await;
    run_script("conformance/notes-client.ts", &url, arguments).await
}

/// Runs the TypeScript `script` against the server at `url` with
/// `arguments`, returning each line it prints as JSON.
async fn run_script(script: &str, url: &str, arguments: &[&str]) -> Vec<Value> {
    let workspace = workspace();
    assert!(
        workspace.join("node_modules/tsx").is_dir(),
        "the TypeScript clients need the npm workspace at {}: run `npm ci` there",
        workspace.display()
    );

    let run = tokio::process::Command::new("node")
        .args(["--conditions=@clerkenwell/source", "--import", "tsx"])
        .arg(workspace.join(script))
        .args(["--url", url])
        .args(arguments)
        .current_dir(&workspace)
        .output();
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), run)
        .await
        .expect("the clients finish in time")
        .expect("the clients run under node");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 output")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_one_typescript_client_sends_reaches_another_typescript_clients_watch() {
    let body = "Written through the TypeScript client.";

    let lines = run_clients(&["--body", body]).await;

    let delivered = lines.last().expect("the delivered note");
    assert_eq!(delivered["body"], body);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reconnecting_typescript_watch_is_sent_nothing_it_holds_and_still_takes_later_edits() {
    let body = "Written after the watcher reconnected.";

    let lines = run_clients(&["--body", body, "--reconnect"]).await;

    let [resumed, delivered] = lines.as_slice() else {
        panic!("a resume report and a note: {lines:?}");
    };
    assert_eq!(resumed["up_to_date"], true);
    assert_eq!(resumed["snapshots"], 0);
    assert_eq!(delivered["body"], body);
}

/// A new server numbers its notes from the first again, so the same note on a
/// new server is a re-seed: one identity, an unrelated history.
#[tokio::test(flavor = "multi_thread")]
async fn a_typescript_client_holding_a_reseeded_note_replaces_its_replica_and_keeps_its_edit() {
    let body = "Written before the note was re-seeded.";
    let (before, _) = serve().await;
    let held = run_script(
        "conformance/reseed-client.ts",
        &before,
        &["--hold", "--body", body],
    )
    .await;
    let held = held.first().expect("what the client held").to_string();

    let (after, server) = serve().await;
    let lines = run_script("conformance/reseed-client.ts", &after, &["--resume", &held]).await;
    let resumed = lines.first().expect("how the client resumed");

    assert_eq!(resumed["replaces_held"], true, "{resumed}");
    assert_eq!(resumed["refused_servers_history"], true, "{resumed}");
    assert_eq!(
        resumed["old_history_refusal"], "collaboration_resync_required",
        "{resumed}"
    );
    let note = server
        .application()
        .service()
        .detail(NOTE, &CollaborationDocumentId::new(NOTE.name, "note-1"))
        .expect("detail")
        .expect("the note");
    assert_eq!(note["body"], body);
    assert_eq!(note["etag"], resumed["accepted_etag"]);
    assert_eq!(note["title"], "Launch plan");
}
