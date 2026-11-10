//! `teamctx mcp --stdio` (plan section 10.1): the same five read-only MCP tools as `/mcp`,
//! over standard input and output, for an agent on the host machine.
//!
//! There is no bearer token on this path. Whoever can start the process can already read
//! the database file, so file-system permissions are the boundary, as for `teamctx q`'s
//! token file. The connection is read-only ([`teamctx_store::Store::open_read_only`]).
//!
//! Transport: rmcp's newline-delimited JSON-RPC over any `AsyncRead`/`AsyncWrite` pair
//! (its `transport-async-rw` feature, already enabled through `server`). The process's
//! standard streams are bridged to an in-memory pipe by two plain threads, so Tokio's
//! `io-std` feature and rmcp's `transport-io` feature are not needed.
//!
//! Errors are fixed text: rmcp's initialize error prints the message it received.

use std::io::{Read, Write};
use std::sync::Arc;

use rmcp::service::serve_server_with_ct;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::mcp::{McpConfig, McpServer};

/// Bytes moved per read on the standard-stream bridge.
const CHUNK: usize = 16 * 1024;

/// Why the stdio server stopped with an error.
#[derive(Debug, Error)]
pub enum StdioError {
    /// The client did not complete the MCP initialize handshake.
    #[error("the MCP client did not initialize the session")]
    Initialize,
    /// The service task failed.
    #[error("the MCP stdio service stopped unexpectedly")]
    Service,
    /// A bridge thread could not be started.
    #[error("could not start the standard input/output bridge")]
    Bridge(#[source] std::io::Error),
}

/// Serves MCP on `reader`/`writer` until the client disconnects or `shutdown` fires.
///
/// # Errors
///
/// [`StdioError`] if the handshake fails or the service stops abnormally.
pub async fn serve_io<R, W>(
    config: McpConfig,
    reader: R,
    writer: W,
    shutdown: CancellationToken,
) -> Result<(), StdioError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let server = McpServer::new(Arc::new(config.into_state()));
    let running = serve_server_with_ct(server, (reader, writer), shutdown)
        .await
        .map_err(|_| StdioError::Initialize)?;
    running
        .waiting()
        .await
        .map(|_| ())
        .map_err(|_| StdioError::Service)
}

/// Serves MCP on the process's standard input and output.
///
/// # Errors
///
/// [`StdioError`] as for [`serve_io`], or if the bridge threads cannot start.
pub async fn serve_stdio(config: McpConfig, shutdown: CancellationToken) -> Result<(), StdioError> {
    let (ours, theirs) = tokio::io::duplex(CHUNK * 4);
    let (mut from_client, mut to_client) = tokio::io::split(theirs);

    // stdin -> pipe. The thread blocks on stdin; it ends at EOF or when the pipe closes.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    std::thread::Builder::new()
        .name("teamctx-mcp-stdin".to_owned())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buffer = vec![0_u8; CHUNK];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if input_tx.blocking_send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .map_err(StdioError::Bridge)?;
    tokio::spawn(async move {
        while let Some(chunk) = input_rx.recv().await {
            if to_client.write_all(&chunk).await.is_err() {
                break;
            }
        }
        // EOF on stdin: closing the pipe ends the MCP session.
        let _ = to_client.shutdown().await;
    });

    // pipe -> stdout, flushed per chunk so each JSON-RPC line leaves promptly.
    let (output_tx, output_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("teamctx-mcp-stdout".to_owned())
        .spawn(move || {
            let mut stdout = std::io::stdout().lock();
            while let Ok(chunk) = output_rx.recv() {
                if stdout
                    .write_all(&chunk)
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
        })
        .map_err(StdioError::Bridge)?;
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; CHUNK];
        loop {
            match from_client.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx.send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let (reader, writer) = tokio::io::split(ours);
    serve_io(config, reader, writer, shutdown).await
}
