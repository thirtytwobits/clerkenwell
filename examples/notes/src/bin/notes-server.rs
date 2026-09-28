//! Serves notes over the projection protocol until stopped, and prints the
//! address clients connect to.

use std::path::PathBuf;

use clap::Parser;
use clerkenwell_axum::ProjectionServer;
use clerkenwell_example_notes::server::NotesServer;

/// Serves notes over the projection protocol.
#[derive(Parser)]
struct Arguments {
    /// The port to listen on; 0 lets the system choose one.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// The directory the notes are kept in; a temporary one when absent.
    #[arg(long)]
    store: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let arguments = Arguments::parse();
    let temporary = tempfile::tempdir()?;
    let store = arguments
        .store
        .unwrap_or_else(|| temporary.path().to_path_buf());
    let server = ProjectionServer::new(NotesServer::new(&store));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", arguments.port)).await?;
    println!("ws://{}/projections", listener.local_addr()?);
    axum::serve(listener, server.router("/projections")).await
}
