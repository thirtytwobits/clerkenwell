//! TypeScript clients sync a note through the Rust notes server.

use clerkenwell_axum::ProjectionServer;
use clerkenwell_conformance::workspace;
use clerkenwell_example_notes::server::NotesServer;
use serde_json::Value;

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_one_typescript_client_sends_reaches_another_typescript_clients_watch() {
    let root = tempfile::tempdir().expect("a store directory");
    let server = ProjectionServer::new(NotesServer::new(root.path()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let url = format!(
        "ws://{}/projections",
        listener.local_addr().expect("an address")
    );
    let router = server.router("/projections");
    tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
    let workspace = workspace();
    assert!(
        workspace.join("node_modules/tsx").is_dir(),
        "the TypeScript clients need the npm workspace at {}: run `npm ci` there",
        workspace.display()
    );
    let body = "Written through the TypeScript client.";

    let run = tokio::process::Command::new("node")
        .args(["--import", "tsx"])
        .arg(workspace.join("conformance/notes-client.ts"))
        .args(["--url", &url, "--body", body])
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
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    let delivered: Value = serde_json::from_str(stdout.lines().last().expect("the delivered note"))
        .expect("a JSON note");
    assert_eq!(delivered["body"], body);
}
