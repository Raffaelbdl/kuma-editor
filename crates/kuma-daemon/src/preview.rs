//! Real-time preview: the C++ `kuma-preview` sidecar (libmlt++) pulls composited frames
//! from the compiled timeline and streams JPEGs; the daemon proxies its WebSocket.
//!
//! The sidecar speaks a tiny line protocol on stdin/stdout:
//!   → `load <path.mlt>` | `seek <frame>` | `play` | `pause` | `quit`
//!   ← `ok …` | `err …` | `frame <frame> <len>` followed by `<len>` JPEG bytes
//!
//! Frames, `ok end` and the engine's exit are fanned out to every preview socket. A
//! socket is bound to one engine process: when it exits every socket closes and the UI
//! reconnects, which starts a fresh engine. Until the engine is built, `snapshot`
//! (single frame via melt) is the fallback the UI uses.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kuma_mlt::UnbakedPolicy;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

use crate::state::{lock, read, AppState};

/// A frame from the engine: JPEG bytes at a timeline frame index.
#[derive(Debug, Clone)]
pub struct PreviewFrame {
    pub frame: i64,
    pub jpeg: Arc<Vec<u8>>,
}

/// What the engine reports to every connected preview.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Frame(PreviewFrame),
    /// Playback ran off the end of the timeline and the engine paused itself.
    End,
    /// The engine process is gone; reconnect to get a new one.
    Exited,
}

pub struct PreviewHandle {
    inner: Mutex<Option<Arc<Engine>>>,
    /// Serializes `ensure` so two first connections cannot both spawn an engine.
    starting: AsyncMutex<()>,
    /// Why the engine is not running, for the UI and `/api/health`.
    pub last_error: Mutex<Option<String>>,
    /// Preview resolution as a fraction of the project size (0.25..=1.0).
    pub scale: Mutex<f64>,
}

impl Default for PreviewHandle {
    fn default() -> Self {
        PreviewHandle { inner: Mutex::new(None), starting: AsyncMutex::new(()), last_error: Mutex::new(None), scale: Mutex::new(0.5) }
    }
}

pub struct Engine {
    stdin: AsyncMutex<ChildStdin>,
    pub events: broadcast::Sender<EngineEvent>,
    _child: Child,
}

/// Largest JPEG the reader will accept from the engine; anything bigger is a protocol error.
const MAX_FRAME_BYTES: usize = 64 << 20;

impl PreviewHandle {
    pub fn get(&self) -> Option<Arc<Engine>> {
        lock(&self.inner).clone()
    }

    pub fn engine_path(state: &AppState) -> PathBuf {
        state.config.repo_root.join("engine/build/kuma-preview")
    }

    /// The running engine, started on demand.
    pub async fn ensure(&self, state: &Arc<AppState>) -> anyhow::Result<Arc<Engine>> {
        let _guard = self.starting.lock().await;
        if let Some(e) = self.get() {
            return Ok(e);
        }
        let started = self.start(state).await;
        *lock(&self.last_error) = started.as_ref().err().map(|e| format!("{e:#}"));
        started
    }

    async fn start(&self, state: &Arc<AppState>) -> anyhow::Result<Arc<Engine>> {
        let bin = Self::engine_path(state);
        if !bin.is_file() {
            anyhow::bail!(
                "preview engine not found at {} — build it with `cmake -S engine -B engine/build && cmake --build engine/build -j` (needs libmlt++-dev, libsdl2-dev, libturbojpeg0-dev)",
                bin.display()
            );
        }
        // MLT's Qt module (qtblend, qtcrop) refuses to load without an X display, so run
        // the engine under Xvfb when headless — same trick as the melt render path.
        let headless = std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none();
        if headless && !has_bin("xvfb-run") {
            anyhow::bail!("no DISPLAY and xvfb-run is not installed; MLT's Qt module needs a display (install `xvfb`)");
        }
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
        let (events, _) = broadcast::channel(8);
        let engine = Arc::new(Engine { stdin: AsyncMutex::new(stdin), events: events.clone(), _child: child });
        *lock(&self.inner) = Some(engine.clone());

        // Reader task: parse the line protocol and fan out frames.
        let st = state.clone();
        let mine = Arc::downgrade(&engine);
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
                    if n > MAX_FRAME_BYTES {
                        tracing::warn!("preview engine: bogus frame length {n}");
                        break;
                    }
                    let mut buf = vec![0u8; n];
                    if rd.read_exact(&mut buf).await.is_err() {
                        break;
                    }
                    let _ = events.send(EngineEvent::Frame(PreviewFrame { frame: f, jpeg: Arc::new(buf) }));
                } else if let Some(e) = l.strip_prefix("err ") {
                    tracing::warn!("preview engine: {e}");
                } else if l == "ok end" {
                    let _ = events.send(EngineEvent::End);
                } else if l.starts_with("ok ready") {
                    ready = true;
                }
            }
            let msg = if ready {
                "preview engine exited; it will be restarted on the next preview connection"
            } else {
                "preview engine exited before becoming ready — run it by hand (engine/build/kuma-preview) and check the daemon log for missing libraries or display errors"
            };
            tracing::warn!("{msg}");
            *lock(&st.preview.last_error) = Some(msg.to_string());
            let _ = events.send(EngineEvent::Exited);
            // Only forget *this* engine; a replacement may already be running.
            let mut inner = lock(&st.preview.inner);
            if inner.as_ref().is_some_and(|e| mine.ptr_eq(&Arc::downgrade(e))) {
                *inner = None;
            }
        });
        Ok(engine)
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

pub fn has_bin(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
}

/// Compile the open sequence for preview (at the preview scale, unbaked overlays skipped
/// — they are live DOM in the UI) and (re)load it into the engine.
pub async fn reload(state: &Arc<AppState>) -> anyhow::Result<PathBuf> {
    let st = state.clone();
    let path = tokio::task::spawn_blocking(move || {
        let project = read(&st.project).clone();
        let scale = (*lock(&st.preview.scale)).clamp(0.25, 1.0);
        let even = |v: f64| ((v.round() as u32).max(2)) & !1;
        let (w, h) = (even(project.width as f64 * scale), even(project.height as f64 * scale));
        // The engine loads one compiled file, so editing a sub-sequence means previewing
        // that sub-sequence: it is compiled as the root, exactly as a render of it would be.
        let sequence = st.current_sequence();
        crate::jobs::compile_project(&st, &project, HashMap::new(), "preview", UnbakedPolicy::Skip, Some((w, h)), Some(sequence))
    })
    .await??;
    state.preview.ensure(state).await?.send(&format!("load {}", path.display())).await?;
    Ok(path)
}
