//! kuma-daemon — the single owner of project state.
//!
//! Both the browser UI and the MCP server are thin clients of this process. Everything
//! UI-relevant (project, revision, undo history, jobs, preview) lives here, which is what
//! makes multi-window and agent+human editing trivial: there is nothing to sync.

pub mod api;
pub mod jobs;
pub mod preview;
pub mod state;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

pub use state::{AppState, Config};

/// Start serving. Resolves when the listener is bound; the server runs in the background.
pub async fn serve(config: Config) -> anyhow::Result<(Arc<AppState>, SocketAddr)> {
    let state = Arc::new(AppState::load(config)?);
    let app = api::router(state.clone());
    let listener = tokio::net::TcpListener::bind(state.config.bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!("kuma daemon listening on http://{addr}");
    let st = state.clone();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("server error: {e}");
        }
        drop(st);
    });
    Ok((state, addr))
}

/// Locate the repository root (for `overlays/` and `node_modules`) from the executable
/// or the current directory.
pub fn find_repo_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KUMA_REPO") {
        return Some(PathBuf::from(p));
    }
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe);
    }
    for start in candidates {
        let mut p = Some(start.as_path());
        while let Some(d) = p {
            if d.join("overlays").is_dir() && d.join("Cargo.toml").is_file() {
                return Some(d.to_path_buf());
            }
            p = d.parent();
        }
    }
    None
}
