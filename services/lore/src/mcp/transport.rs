//! Streamable-HTTP transport wiring using `rmcp` + `axum`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tracing::info;

use crate::mcp::{CorpusRegistry, LoreServer};

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub bind: SocketAddr,
    pub path: String,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7331".parse().unwrap(),
            path: "/mcp".to_string(),
        }
    }
}

/// Run the Streamable HTTP server. Blocks until the listener errors.
pub async fn serve_http(
    registry: CorpusRegistry,
    opts: ServeOptions,
) -> Result<(), std::io::Error> {
    let service = StreamableHttpService::new(
        move || Ok(LoreServer::new(registry.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let app = Router::new().nest_service(&opts.path, service);
    let listener = TcpListener::bind(opts.bind).await?;
    info!(addr = %opts.bind, path = %opts.path, "lore mcp server listening");
    axum::serve(listener, app).await
}
