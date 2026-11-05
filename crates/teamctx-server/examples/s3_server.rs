//! Runs the spike S3 server so a real MCP client (Claude Code, T03b) can connect to it.
//!
//! ```sh
//! TEAMCTX_S3_TOKEN=tctx_test_local_only cargo run -p teamctx-server --example s3_server
//! ```
//!
//! Binds `127.0.0.1:8787` (override with `TEAMCTX_S3_ADDR`). The token comes from the
//! environment and is never printed.

use std::{env, net::SocketAddr};

use teamctx_server::{HttpConfig, router, secret::Secret};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = env::var("TEAMCTX_S3_TOKEN")
        .map_err(|_| "set TEAMCTX_S3_TOKEN to a throwaway test token")?;
    let addr: SocketAddr = env::var("TEAMCTX_S3_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8787".to_owned())
        .parse()?;
    let shutdown = CancellationToken::new();
    let app = router(HttpConfig {
        token: Secret::new(token),
        allowed_hosts: vec!["127.0.0.1".to_owned(), "localhost".to_owned()],
        shutdown: shutdown.clone(),
    });
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("MCP endpoint: http://{addr}/mcp (Ctrl+C to stop)");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.cancel();
        })
        .await?;
    Ok(())
}
