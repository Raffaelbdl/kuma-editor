//! kuma-mcp — the agent-facing surface. A stdio MCP server that proxies to the daemon so
//! Claude Code and the browser UI always see the same project state.

pub mod client;
pub mod server;

pub use client::Client;
pub use server::KumaServer;

/// Serve MCP over stdio against a daemon at `daemon_url`.
pub async fn run_stdio(daemon_url: String) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let server = KumaServer::new(Client::new(daemon_url));
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
