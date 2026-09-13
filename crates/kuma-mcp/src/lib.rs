//! kuma-mcp — the agent-facing surface. A stdio MCP server that proxies to the daemon so
//! Claude Code and the browser UI always see the same project state.

pub mod client;
pub mod server;

pub use client::Client;
pub use server::KumaServer;

/// Serve MCP over stdio against a daemon at `daemon_url`.
/// `note` is surfaced in the server instructions and the `health` tool — used to tell the
/// agent when it is talking to a private daemon rather than the one behind the browser UI.
pub async fn run_stdio(daemon_url: String, note: Option<String>) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let server = KumaServer::new(Client::new(daemon_url), note);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
