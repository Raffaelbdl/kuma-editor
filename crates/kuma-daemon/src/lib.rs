//! kuma-daemon — the single owner of project state.
//!
//! Both the browser UI and the MCP server are thin clients of this process. Everything
//! UI-relevant (project, revision, undo history, jobs, preview) lives here, which is what
//! makes multi-window and agent+human editing trivial: there is nothing to sync.

pub mod api;
pub mod cache;
pub mod fonts;
pub mod jobs;
pub mod overlays;
pub mod preview;
pub mod state;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

pub use state::{add_missing, copy_tree, default_library_dirs, AppState, Config};

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

/// The repository root (for `node_modules/hyperframes`, `ui/dist`, `engine/build`):
/// `KUMA_REPO`, else the nearest ancestor of the executable or the cwd that holds
/// `node_modules/hyperframes`.
pub fn find_repo_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KUMA_REPO").map(PathBuf::from).filter(|p| p.is_dir()) {
        return Some(p);
    }
    let is_repo = |d: &std::path::Path| d.join("node_modules/hyperframes").is_dir();
    [std::env::current_exe().ok(), std::env::current_dir().ok()]
        .into_iter()
        .flatten()
        .find_map(|start| start.ancestors().find(|d| is_repo(d)).map(|d| d.to_path_buf()))
}
