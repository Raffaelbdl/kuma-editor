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

pub struct PreviewHandle {
    inner: Mutex<Option<Arc<Engine>>>,
    /// Why the engine is not running, for the UI and `/api/health`.
    pub last_error: Mutex<Option<String>>,
    /// Preview resolution as a fraction of the project size (0.25..=1.0).
    pub scale: Mutex<f64>,
}

impl Default for PreviewHandle {
    fn default() -> Self {
        PreviewHandle { inner: Mutex::new(None), last_error: Mutex::new(None), scale: Mutex::new(0.5) }
    }
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

    pub fn engine_path(state: &AppState) -> PathBuf {
        state.config.preview_engine.clone().unwrap_or_else(|| state.config.repo_root.join("engine/build/kuma-preview"))
    }

    /// Start (or reuse) the engine process.
    pub async fn ensure(&self, state: &Arc<AppState>) -> anyhow::Result<Arc<Engine>> {
        if let Some(e) = self.get() {
            return Ok(e);
        }
        match self.start(state).await {
            Ok(e) => {
                *self.last_error.lock().unwrap() = None;
                Ok(e)
            }
            Err(e) => {
                *self.last_error.lock().unwrap() = Some(format!("{e:#}"));
                Err(e)
            }
        }
    }

    async fn start(&self, state: &Arc<AppState>) -> anyhow::Result<Arc<Engine>> {
        let bin = Self::engine_path(state);
        if !bin.is_file() {
            anyhow::bail!(
                "preview engine not found at {} — build it with `cmake -S engine -B engine/build && cmake --build engine/build -j` (needs libmlt++-dev, libsdl2-dev, libturbojpeg0-dev)",
                bin.display()
            );
        }
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() && !has_bin("xvfb-run") {
            anyhow::bail!("no DISPLAY and xvfb-run is not installed; MLT's Qt module needs a display (install `xvfb`)");
        }
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
        let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("cannot start {}: {e}", bin.display()))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (frames, _) = broadcast::channel(8);
        let (positions, _) = broadcast::channel(64);
        let engine = Arc::new(Engine { stdin: AsyncMutex::new(stdin), frames: frames.clone(), positions: positions.clone(), _child: AsyncMutex::new(child) });
        *self.inner.lock().unwrap() = Some(engine.clone());

        // Reader task: parse the line protocol and fan out frames.
        let st = state.clone();
        tokio::spawn(async move {
            let mut rd = BufReader::new(stdout);
            let mut line = String::new();
            let mut ready = false;
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
                } else if l.starts_with("ok ready") {
                    ready = true;
                }
            }
            let msg = if ready {
                "preview engine exited; it will be restarted on the next preview connection".to_string()
            } else {
                "preview engine exited before becoming ready — run it by hand (engine/build/kuma-preview) and check the daemon log for missing libraries or display errors".to_string()
            };
            tracing::warn!("{msg}");
            *st.preview.last_error.lock().unwrap() = Some(msg);
            *st.preview.inner.lock().unwrap() = None;
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

fn has_bin(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
}

/// Compile the current project for preview (at the preview scale, unbaked overlays
/// skipped — they are live DOM in the UI) and (re)load it into the engine.
pub async fn reload(state: &Arc<AppState>) -> anyhow::Result<PathBuf> {
    let project = state.project.read().unwrap().clone();
    let scale = (*state.preview.scale.lock().unwrap()).clamp(0.25, 1.0);
    let even = |v: f64| ((v.round() as u32).max(2)) & !1;
    let (w, h) = (even(project.width as f64 * scale), even(project.height as f64 * scale));
    let path = crate::jobs::compile_project(state, &project, HashMap::new(), "preview", UnbakedPolicy::Skip, Some((w, h)))?;
    if let Ok(engine) = state.preview.ensure(state).await {
        engine.send(&format!("load {}", path.display())).await?;
    }
    Ok(path)
}
