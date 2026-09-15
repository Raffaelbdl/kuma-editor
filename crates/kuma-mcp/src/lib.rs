//! kuma-mcp — an MCP server (stdio) that exposes the daemon to agents.
//!
//! Every tool is a thin call into the daemon's HTTP API, so the agent and the UI always
//! see the same state; `sample_frames` returns rendered frames as image content.

pub mod client;
pub mod server;

pub use client::Client;
pub use server::KumaServer;

/// Serve MCP over stdio against the daemon at `daemon_url` until the client disconnects.
/// `note` is surfaced to the agent when this server had to start a private daemon.
pub async fn run_stdio(daemon_url: String, note: Option<String>) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let server = KumaServer::new(Client::new(daemon_url), note);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
