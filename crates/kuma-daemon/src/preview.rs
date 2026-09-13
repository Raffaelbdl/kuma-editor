//! Real-time preview: the C++ `kuma-preview` sidecar (libmlt++) pulls composited frames
//! from the compiled timeline and streams JPEGs; the daemon proxies its WebSocket.
//!
//! The sidecar speaks a tiny line protocol on stdin/stdout:
//!   → `load <path.mlt>` | `seek <frame>` | `play` | `pause` | `quit`
//!   ← `ok …` | `err …` | `pos <frame>` | `frame <frame> <len>` followed by `<len>` JPEG bytes
//!
//! Until the engine is built, `snapshot` (single frame via melt) is the fallback the UI uses.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kuma_mlt::UnbakedPolicy;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

use crate::state::AppState;

/// A frame from the engine: JPEG bytes at a timeline frame index.
#[derive(Debug, Clone)]
pub struct PreviewFrame {
    pub frame: i64,
    pub jpeg: Arc<Vec<u8>>,
}

#[derive(Default)]
pub struct PreviewHandle {
    inner: Mutex<Option<Arc<Engine>>>,
}

pub struct Engine {
    stdin: AsyncMutex<ChildStdin>,
    pub frames: broadcast::Sender<PreviewFrame>,
    pub positions: broadcast::Sender<i64>,
    _child: AsyncMutex<Child>,
}

impl PreviewHandle {
    pub fn get(&self) -> Option<Arc<Engine>> {
        self.inner.lock().unwrap().clone()
    }

    /// Start (or reuse) the engine process.
    pub async fn ensure(&self, state: &Arc<AppState>) -> anyhow::Result<Arc<Engine>> {
        if let Some(e) = self.get() {
            return Ok(e);
        }
        let bin = state
            .config
            .preview_engine
            .clone()
            .or_else(|| {
                let p = state.config.repo_root.join("engine/build/kuma-preview");
                p.is_file().then_some(p)
            })
            .ok_or_else(|| anyhow::anyhow!("preview engine not built (see engine/README.md)"))?;
        // MLT's Qt module (qtblend, qtcrop) refuses to load without an X display, so run
        // the engine under Xvfb when headless — same trick as the melt render path.
        let headless = std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none();
        let mut cmd = if headless {
            let mut c = Command::new("xvfb-run");
            c.arg("-a").arg(&bin);
            c
        } else {
            Command::new(&bin)
        };
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit());
        cmd.env("LC_NUMERIC", "C").env("SDL_VIDEODRIVER", "dummy");
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (frames, _) = broadcast::channel(8);
        let (positions, _) = broadcast::channel(64);
        let engine = Arc::new(Engine { stdin: AsyncMutex::new(stdin), frames: frames.clone(), positions: positions.clone(), _child: AsyncMutex::new(child) });
        *self.inner.lock().unwrap() = Some(engine.clone());

        // Reader task: parse the line protocol and fan out frames.
        tokio::spawn(async move {
            let mut rd = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match rd.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let l = line.trim_end();
                if let Some(rest) = l.strip_prefix("frame ") {
                    let mut it = rest.split_whitespace();
                    let (Some(f), Some(n)) = (it.next(), it.next()) else { continue };
                    let (Ok(f), Ok(n)) = (f.parse::<i64>(), n.parse::<usize>()) else { continue };
                    let mut buf = vec![0u8; n];
                    if rd.read_exact(&mut buf).await.is_err() {
                        break;
                    }
                    let _ = frames.send(PreviewFrame { frame: f, jpeg: Arc::new(buf) });
                    let _ = positions.send(f);
                } else if let Some(rest) = l.strip_prefix("pos ") {
                    if let Ok(f) = rest.trim().parse::<i64>() {
                        let _ = positions.send(f);
                    }
                } else if let Some(e) = l.strip_prefix("err ") {
                    tracing::warn!("preview engine: {e}");
                }
            }
            tracing::info!("preview engine exited");
        });
        Ok(engine)
    }

    pub fn stop(&self) {
        *self.inner.lock().unwrap() = None;
    }
}

impl Engine {
    pub async fn send(&self, cmd: &str) -> anyhow::Result<()> {
        let mut s = self.stdin.lock().await;
        s.write_all(cmd.as_bytes()).await?;
        s.write_all(b"\n").await?;
        s.flush().await?;
        Ok(())
    }
}

/// Compile the current project for preview (half resolution, unbaked overlays skipped —
/// they are live DOM in the UI) and (re)load it into the engine.
pub async fn reload(state: &Arc<AppState>) -> anyhow::Result<PathBuf> {
    let project = state.project.read().unwrap().clone();
    let (w, h) = ((project.width / 2).max(2) & !1, (project.height / 2).max(2) & !1);
    let path = crate::jobs::compile_project(state, &project, HashMap::new(), "preview", UnbakedPolicy::Skip, Some((w, h)))?;
    if let Ok(engine) = state.preview.ensure(state).await {
        engine.send(&format!("load {}", path.display())).await?;
    }
    Ok(path)
}
