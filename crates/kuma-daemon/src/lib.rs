//! kuma-daemon — the single owner of project state.
//!
//! Both the browser UI and the MCP server are thin clients of this process. Everything
//! UI-relevant (project, revision, undo history, jobs, preview) lives here, which is what
//! makes multi-window and agent+human editing trivial: there is nothing to sync.

pub mod api;
pub mod cache;
pub mod fonts;
pub mod jobs;
pub mod media;
pub mod overlays;
pub mod preview;
pub mod state;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use state::{add_missing, copy_tree, default_library_dirs, library_dirs, AppState, Config};

/// Start serving. Resolves when the listener is bound; the server runs in the background.
pub async fn serve(config: Config) -> anyhow::Result<(Arc<AppState>, SocketAddr)> {
    let state = Arc::new(AppState::load(config)?);
    let shipped = state.config.repo_root.join("library");
    if state.config.library_dirs.contains(&shipped) {
        tracing::info!("no library installed; reading the starter set in {} (`kuma library` installs it)", shipped.display());
    }
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

/// A repository root, and where it was found, for `kuma doctor` to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRoot {
    pub path: PathBuf,
    pub from: String,
}

/// The repository root (holding `node_modules/hyperframes`, `ui/dist`, `engine/build`):
/// `KUMA_REPO`, else the nearest ancestor of the executable or the cwd that is one, else the
/// `KUMA_REPO` that `kuma mcp-config` wrote in the `.mcp.json` beside `project_file` (for a
/// `kuma` installed out of the checkout), else the source tree this binary was built from.
pub fn locate_repo_root(project_file: Option<&Path>) -> Option<RepoRoot> {
    let env = std::env::var_os("KUMA_REPO").map(PathBuf::from);
    let starts = [(std::env::current_exe().ok(), "the executable's location"), (std::env::current_dir().ok(), "the working directory")];
    let mcp_json = project_file.and_then(Path::parent).map(|d| d.join(".mcp.json"));
    repo_root_from(env, &starts, mcp_json.as_deref(), Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// [`locate_repo_root`] with its inputs given, so the order can be tested.
fn repo_root_from(env: Option<PathBuf>, starts: &[(Option<PathBuf>, &str)], mcp_json: Option<&Path>, built_in: &Path) -> Option<RepoRoot> {
    let found = |path: &Path, from: &str| Some(RepoRoot { path: path.to_path_buf(), from: from.to_string() });
    if let Some(p) = env.filter(|p| p.is_dir()) {
        return found(&p, "KUMA_REPO");
    }
    let is_repo = |d: &Path| d.join("node_modules/hyperframes").is_dir();
    for (start, from) in starts {
        if let Some(d) = start.as_deref().and_then(|s| s.ancestors().find(|d| is_repo(d))) {
            return found(d, from);
        }
    }
    if let Some(file) = mcp_json {
        let named = std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        let named = named.as_ref().and_then(|v| v["mcpServers"]["kuma"]["env"]["KUMA_REPO"].as_str()).map(PathBuf::from);
        if let Some(p) = named.filter(|p| p.is_dir()) {
            return found(&p, &format!("KUMA_REPO in {}", file.display()));
        }
    }
    built_in.ancestors().find(|d| is_repo(d)).and_then(|d| found(d, "the source tree it was built from"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder that looks like a checkout, under `root`.
    fn checkout(root: &Path, name: &str) -> PathBuf {
        let d = root.join(name);
        std::fs::create_dir_all(d.join("node_modules/hyperframes")).unwrap();
        d
    }

    /// Asked for wins; then a checkout above the executable or the cwd; then the one the
    /// project's `.mcp.json` names; then the tree the binary was built in.
    #[test]
    fn the_repo_root_is_found_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let (env, above, named, built) = (checkout(tmp.path(), "env"), checkout(tmp.path(), "above"), checkout(tmp.path(), "named"), checkout(tmp.path(), "built"));
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mcp = project.join(".mcp.json");
        let config = serde_json::json!({ "mcpServers": { "kuma": { "command": "kuma", "env": { "KUMA_REPO": named } } } });
        std::fs::write(&mcp, config.to_string()).unwrap();
        let exe = above.join("target/release/kuma");
        let nowhere = [(Some(tmp.path().join("bin/kuma")), "the executable's location"), (Some(project.clone()), "the working directory")];
        let built_in = built.join("crates/kuma-daemon");
        let from = |env: Option<PathBuf>, starts: &[(Option<PathBuf>, &str)], mcp: Option<&Path>| repo_root_from(env, starts, mcp, &built_in).map(|r| (r.path, r.from));

        assert_eq!(from(Some(env.clone()), &[(Some(exe.clone()), "the executable's location")], Some(&mcp)), Some((env, "KUMA_REPO".into())));
        assert_eq!(from(None, &[(Some(exe), "the executable's location")], Some(&mcp)), Some((above, "the executable's location".into())));
        assert_eq!(from(None, &nowhere, Some(&mcp)), Some((named, format!("KUMA_REPO in {}", mcp.display()))));
        assert_eq!(from(None, &nowhere, None), Some((built.clone(), "the source tree it was built from".into())));
        // A `.mcp.json` naming a checkout that has gone is passed over, not trusted.
        std::fs::write(&mcp, serde_json::json!({ "mcpServers": { "kuma": { "env": { "KUMA_REPO": tmp.path().join("gone") } } } }).to_string()).unwrap();
        assert_eq!(from(None, &nowhere, Some(&mcp)), Some((built, "the source tree it was built from".into())));
        assert_eq!(repo_root_from(None, &nowhere, Some(&mcp), tmp.path()), None);
    }
}
