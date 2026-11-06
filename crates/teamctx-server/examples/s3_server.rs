//! Runs the MCP server so a real MCP client (for example Claude Code) can connect to it.
//!
//! ```sh
//! teamctx demo --db scratch.db                      # fictional demo record
//! teamctx token create local-test --db scratch.db   # prints a token once
//! TEAMCTX_DB=scratch.db cargo run -p teamctx-server --example s3_server
//! ```
//!
//! Binds `127.0.0.1:8787` (override with `TEAMCTX_S3_ADDR`). Tokens are checked against
//! the database at `TEAMCTX_DB`; nothing secret is printed.

use std::{env, net::SocketAddr, sync::Arc};

use teamctx_server::{HttpConfig, auth::StoreTokens, mcp::McpConfig, router, serve};
use teamctx_store::{Store, Writer};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = env::var("TEAMCTX_DB").map_err(|_| "set TEAMCTX_DB to a scratch database path")?;
    let addr: SocketAddr = env::var("TEAMCTX_S3_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8787".to_owned())
        .parse()?;
    let writer = Writer::spawn(Store::open(&db)?)?;
    let tokens = StoreTokens::new(Store::open_read_only(&db)?, writer.client());
    let shutdown = CancellationToken::new();
    let app = router(HttpConfig {
        tokens: Arc::new(tokens),
        allowed_hosts: vec!["127.0.0.1".to_owned(), "localhost".to_owned()],
        shutdown: shutdown.clone(),
        mcp: McpConfig::new(Store::open_read_only(&db)?),
    });
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("MCP endpoint: http://{addr}/mcp (Ctrl+C to stop)");
    let stop = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.cancel();
    });
    serve(listener, app, shutdown).await?;
    writer.shutdown()?;
    Ok(())
}
