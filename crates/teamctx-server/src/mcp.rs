//! The MCP service mounted at `/mcp`. Spike S3 ships one read-only tool, `ping`; the five
//! real tools arrive in M4.

use rmcp::{
    ServerHandler,
    handler::server::router::tool::ToolRouter,
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};

/// Placeholder MCP handler used to prove the transport and auth stack.
#[derive(Clone, Debug)]
pub struct PingServer {
    tool_router: ToolRouter<Self>,
}

impl PingServer {
    /// Builds the handler with its tool router.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for PingServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl PingServer {
    /// Replies with a fixed, fictional line. Reads nothing and writes nothing.
    #[tool(
        description = "Check that the teamctx server is reachable. Returns a fixed line.",
        annotations(read_only_hint = true)
    )]
    async fn ping(&self) -> String {
        "pong from the Example Team record".to_owned()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PingServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("teamctx", env!("CARGO_PKG_VERSION")))
    }
}
