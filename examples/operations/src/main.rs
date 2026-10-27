//! The tasks' operations served as Clapham's reference process protocol
//! describes: a CLI that calls its daemon over MCP, and an MCP server over
//! stdio or Streamable HTTP.
//!
//! In fixture mode, which Clapham's conformance harness sets, a grant is the
//! scripted claim snapshot the harness passes; otherwise the server verifies
//! bearer tokens by OAuth introspection.

use std::error::Error;
use std::io::Write;
use std::process::{ExitCode, Stdio};

use clapham::{Bounds, Cancellation, ComposeOptions, ComposedSet, ExecutionMode, OAuthContext};
use clapham_cli::{Cli, McpBackend};
use clapham_conformance::scripted_access;
use clapham_mcp_server::{Authenticator, Credential, HttpServer, OAuthIntrospector, Server};
use clerkenwell_example_operations::conformance::{fixture, Seed};
use clerkenwell_ops::DOCUMENT_RESOURCE;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// The name the CLI is invoked as.
const NAME: &str = "tasks";

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let outcome = runtime.block_on(run());
    runtime.shutdown_background();
    match outcome {
        Ok(code) => ExitCode::from(code),
        Err(error) => ExitCode::from(clapham_cli::write_refusal(
            &clapham::Refusal::not_dispatched(
                "application_configuration",
                clapham::Category::Configuration,
                error.to_string(),
            ),
            std::io::stderr(),
        )),
    }
}

/// The tasks of the seed the environment names, or of a freshly minted one,
/// composed as a daemon serves them.
fn composed() -> Result<ComposedSet> {
    Ok(fixture(Seed::from_environment()?).compose(ComposeOptions {
        execution: ExecutionMode::Daemon,
        ..Default::default()
    })?)
}

fn fixture_mode() -> bool {
    std::env::var("CLAPHAM_FIXTURE_MODE").as_deref() == Ok("1")
}

fn fixture_authority() -> Result<OAuthContext> {
    let claims: Value = serde_json::from_str(&std::env::var("CLAPHAM_FIXTURE_ACCESS")?)?;
    scripted_access(&claims).ok_or_else(|| "invalid scripted fixture authority".into())
}

fn introspector(set: &ComposedSet) -> Result<OAuthIntrospector> {
    OAuthIntrospector::new(
        set.oauth_resource()
            .expect("the configuration binds OAuth")
            .clone(),
        std::env::var("CLAPHAM_INTROSPECTION_URL")?,
        std::env::var("CLAPHAM_OAUTH_CLIENT_ID")?,
        std::env::var("CLAPHAM_OAUTH_CLIENT_SECRET")?,
    )
}

fn http_server(set: ComposedSet) -> Result<HttpServer> {
    let server = if fixture_mode() {
        HttpServer::new(set, |credential: &Credential| {
            let claims = credential.authorization()?.strip_prefix("Bearer ")?;
            scripted_access(&serde_json::from_str::<Value>(claims).ok()?)
        })?
    } else {
        let oauth = introspector(&set)?;
        HttpServer::new(set, oauth)?
    };
    Ok(server.resources([DOCUMENT_RESOURCE])?)
}

async fn run() -> Result<u8> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("serve") {
        if std::env::var("CLAPHAM_TRANSPORT").as_deref() == Ok("stdio") {
            return cli_over_stdio().await;
        }
        if fixture_mode() {
            return cli_over_fixture_daemon().await;
        }
        let backend = McpBackend::http(
            std::env::var("CLAPHAM_ENDPOINT")?,
            std::env::var("CLAPHAM_ACCESS_TOKEN")?,
        )
        .await?;
        return Ok(Cli::connect(NAME, backend, Bounds::default())
            .await?
            .run()
            .await);
    }
    let set = composed()?;
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["serve", "--stdio"] => {
            let access = if fixture_mode() {
                fixture_authority()?
            } else {
                introspector(&set)?
                    .authenticate(&Credential::new(format!(
                        "Bearer {}",
                        std::env::var("CLAPHAM_ACCESS_TOKEN")?
                    )))
                    .await
                    .ok_or("the launch token was not verified")?
            };
            Server::new(set, access)?
                .resources([DOCUMENT_RESOURCE])?
                .serve_stdio()
                .await?
                .waiting()
                .await?;
        }
        ["serve", "--http", address] => {
            let listener = tokio::net::TcpListener::bind(address).await?;
            if fixture_mode() && !listener.local_addr()?.ip().is_loopback() {
                return Err("scripted fixture authentication requires loopback".into());
            }
            let app = http_server(set)?.into_router();
            println!(
                "{}",
                json!({ "address": listener.local_addr()?.to_string() })
            );
            std::io::stdout().flush()?;
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await?;
        }
        _ => return Err("expected serve --stdio or serve --http ADDR".into()),
    }
    Ok(0)
}

/// Serve freshly seeded tasks on a loopback listener and run the CLI against
/// them, under the scripted grant.
async fn cli_over_fixture_daemon() -> Result<u8> {
    let access = fixture_authority()?;
    let server = http_server(composed()?)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let stop = Cancellation::new();
    let stopped = stop.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, server.into_router())
            .with_graceful_shutdown(async move { stopped.cancelled().await })
            .await
    });
    let backend =
        McpBackend::http(format!("{endpoint}/mcp"), serde_json::to_string(&access)?).await?;
    let result = Cli::connect(NAME, backend, Bounds::default())
        .await?
        .run()
        .await;
    stop.cancel();
    task.await??;
    Ok(result)
}

/// Launch this executable's stdio server, which verifies the launch
/// credential in the inherited environment, and run the CLI over its
/// standard streams.
async fn cli_over_stdio() -> Result<u8> {
    let mut server = tokio::process::Command::new(std::env::current_exe()?)
        .args(["serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let input = server.stdin.take().ok_or("missing server stdin")?;
    let output = server.stdout.take().ok_or("missing server stdout")?;
    let backend = McpBackend::stdio(output, input).await?;
    // Dropping the CLI closes the connection and the server's stdin.
    let result = Cli::connect(NAME, backend, Bounds::default())
        .await?
        .run()
        .await;
    if !server.wait().await?.success() {
        return Err("the stdio server failed".into());
    }
    Ok(result)
}

async fn shutdown() {
    let mut stdin = tokio::io::stdin();
    let mut byte = [0];
    tokio::select! { _ = stdin.read(&mut byte) => {}, _ = tokio::signal::ctrl_c() => {} }
}
