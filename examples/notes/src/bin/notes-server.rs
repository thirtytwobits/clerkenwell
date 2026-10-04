//! Serves notes over the projection protocol until stopped, and prints the
//! address clients connect to.

use clap::Parser;
use clerkenwell_axum::ProjectionServer;
use clerkenwell_example_notes::server::{NotesServer, PUBLICATION_WINDOW};

/// Serves notes over the projection protocol.
#[derive(Parser)]
struct Arguments {
    /// The port to listen on; 0 lets the system choose one.
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let arguments = Arguments::parse();
    let server = ProjectionServer::new(NotesServer::default(), PUBLICATION_WINDOW);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", arguments.port)).await?;
    println!("ws://{}/projections", listener.local_addr()?);
    axum::serve(listener, server.router("/projections")).await
}
