//! Real-time preview: the C++ `kuma-preview` sidecar (libmlt++) pulls composited frames
//! from the compiled timeline and streams JPEGs; the daemon proxies its WebSocket.
//!
//! The sidecar speaks a tiny line protocol on stdin/stdout:
//!   → `load-video <path.mlt>` | `load-audio <path.mlt>` | `seek <frame>` | `play` | `pause` | `quit`
//!   ← `ok …` | `err …` | `frame <frame> <len>` followed by `<len>` JPEG bytes
//!
//! Frames, what became of a `play` (`ok play`, `ok waiting …`, `refused …`),
//! the engine's other errors, `ok end` and the engine's exit are fanned out to every
//! preview socket, so a client never has to guess why pressing play showed nothing. A
//! socket is bound to one engine process: when it exits every socket closes and the UI
//! reconnects, which starts a fresh engine. Until the engine is built, `snapshot`
//! (single frame via melt) is the fallback the UI uses.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kuma_mlt::Streams;
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
    /// Playback started: the engine's answer to `play`.
    Playing,
    /// A `play` the engine holds until the timeline it is loading is in, and why.
    Waiting(String),
    /// A `play` the engine refused, so nothing is playing.
    Refused(String),
    /// Something else the engine could not do: a graph that would not open, a frame it
    /// could not render. At most one every few seconds, since a broken frame repeats.
    Error(String),
    /// Playback ran off the end of the timeline and the engine paused itself.
    End,
    /// The engine process is gone; reconnect to get a new one.
    Exited,
}

impl EngineEvent {
    /// The status line a preview socket says for this event: every one but a frame, which
    /// goes as bytes, and the engine's exit, which closes the socket.
    pub fn status(&self) -> Option<serde_json::Value> {
        use serde_json::json;
        Some(match self {
            EngineEvent::End => json!({ "end": true }),
            EngineEvent::Playing => json!({ "playing": true }),
            EngineEvent::Waiting(why) => json!({ "waiting": why }),
            EngineEvent::Refused(why) => json!({ "refused": why }),
            EngineEvent::Error(e) => json!({ "engine": e }),
            EngineEvent::Frame(_) | EngineEvent::Exited => return None,
        })
    }
}

pub struct PreviewHandle {
    inner: Mutex<Option<Arc<Engine>>>,
    /// Serializes `ensure` so two first connections cannot both spawn an engine.
    starting: AsyncMutex<()>,
    /// Why the engine is not running, for the UI and `/api/health`.
    pub last_error: Mutex<Option<String>>,
    /// Preview resolution as a fraction of the project size: 0.5 or 1 (`set_scale`).
    pub scale: Mutex<f64>,
    /// The picture's graph and the sound's, each reloaded only when an edit changes it.
    halves: [Half; 2],
    /// Compiles performed. Only diagnostic — it is how the coalescing is observed, here
    /// and in `tests/preview.rs`.
    pub compiles: AtomicU64,
    /// One compile at a time. Whoever holds this does the work; `pending` is how a caller
    /// that arrived mid-compile asks for one more pass.
    compiling: AsyncMutex<()>,
    pending: AtomicBool,
    /// The background bake of nested overlays the cache did not have.
    behind: Mutex<Behind>,
    /// The background making of proxies (`proxies.rs`).
    pub making: Mutex<crate::proxies::Making>,
}

/// A compile never waits on hyperframes: it uses the bakes the cache holds and hands the
/// missing ones to one background job at a time (`preview-bake`), which clients hear end and
/// answer with a reload, as they answer an edit. `tried` keeps an overlay that will not bake
/// from being retried on every compile; an edit or a forced reload clears it.
#[derive(Default)]
struct Behind {
    running: bool,
    revision: u64,
    tried: std::collections::HashSet<String>,
}

impl Default for PreviewHandle {
    fn default() -> Self {
        PreviewHandle {
            inner: Mutex::new(None),
            starting: AsyncMutex::new(()),
            last_error: Mutex::new(None),
            scale: Mutex::new(0.5),
            halves: [Half::new(Streams::Video, "preview-video", "load-video"), Half::new(Streams::Audio, "preview-audio", "load-audio")],
            compiles: AtomicU64::new(0),
            compiling: AsyncMutex::new(()),
            pending: AtomicBool::new(false),
            behind: Mutex::new(Behind::default()),
            making: Mutex::new(Default::default()),
        }
    }
}

/// One of the engine's two graphs.
struct Half {
    stream: Streams,
    /// Written to `.kuma/<name>.mlt`.
    name: &'static str,
    /// The engine command that loads it.
    cmd: &'static str,
    /// Hash of the document the engine is holding, so an edit that compiles to the same
    /// one costs no rebuild. Cleared whenever a fresh engine starts, which holds nothing.
    loaded: Mutex<Option<u64>>,
    /// Documents handed to an engine. `reload` reports what it changed by comparing this
    /// across the call, not by comparing hashes: a restarted engine has to be given the
    /// same document again, and a caller whose work was done by a coalesced run still
    /// wants to hear that the engine was updated.
    loads: AtomicU64,
}

impl Half {
    fn new(stream: Streams, name: &'static str, cmd: &'static str) -> Half {
        Half { stream, name, cmd, loaded: Mutex::new(None), loads: AtomicU64::new(0) }
    }
}

/// What a reload did. `changed` is false when the project compiled to the documents the
/// engine already had, so it was left alone; `video` and `audio` say which of the two
/// graphs it was given.
#[derive(Debug, Clone)]
pub struct Reloaded {
    /// The picture's graph; the sound's is beside it.
    pub path: PathBuf,
    pub changed: bool,
    pub video: bool,
    pub audio: bool,
    /// Overlays of nested sequences in the picture, from the cache.
    pub baked: usize,
    /// Overlay clips of nested sequences still to bake: missing from the picture until the
    /// background bake (a `preview-bake` job) is done.
    pub baking: Vec<String>,
}

pub struct Engine {
    stdin: AsyncMutex<ChildStdin>,
    pub events: broadcast::Sender<EngineEvent>,
    _child: Child,
}

/// Largest JPEG the reader will accept from the engine; anything bigger is a protocol error.
const MAX_FRAME_BYTES: usize = 64 << 20;

impl PreviewHandle {
    /// Set the preview's one resolution setting: ½, which decodes proxies, or full, which
    /// decodes the media. Anything short of full is ½, since proxies are made for exactly
    /// that size (`proxies::target`).
    pub fn set_scale(&self, scale: f64) {
        *lock(&self.scale) = if scale < 1.0 { 0.5 } else { 1.0 };
    }

    pub fn get(&self) -> Option<Arc<Engine>> {
        lock(&self.inner).clone()
    }

    /// Forget the running engine, so the next use starts a fresh one. This is what the
    /// reader task does when the process exits; having it here is also what a "restart the
    /// preview" action would call.
    pub fn forget(&self) {
        *lock(&self.inner) = None;
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
        // A new process holds no timeline, whatever the old one had.
        for half in &self.halves {
            *lock(&half.loaded) = None;
        }

        // Reader task: parse the line protocol and fan out frames.
        let st = state.clone();
        let mine = Arc::downgrade(&engine);
        tokio::spawn(async move {
            let mut rd = BufReader::new(stdout);
            let mut line = String::new();
            let mut ready = false;
            let mut said: Option<std::time::Instant> = None;
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
                } else if let Some(why) = l.strip_prefix("refused ") {
                    let _ = events.send(EngineEvent::Refused(why.to_string()));
                } else if let Some(e) = l.strip_prefix("err ") {
                    tracing::warn!("preview engine: {e}");
                    if said.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(5)) {
                        said = Some(std::time::Instant::now());
                        let _ = events.send(EngineEvent::Error(e.to_string()));
                    }
                } else if l == "ok play" {
                    let _ = events.send(EngineEvent::Playing);
                } else if let Some(w) = l.strip_prefix("ok waiting ") {
                    let _ = events.send(EngineEvent::Waiting(w.to_string()));
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

/// Bring the engine up to date with the open sequence, and say whether that meant giving
/// it a new timeline.
///
/// Two things keep this cheap enough to call on every edit. The compile is compared
/// against what the engine is already holding, so an edit that cannot change the picture
/// costs a compile (milliseconds) rather than a graph rebuild — which is why no client
/// has to predict what the compiler depends on. And compiles are coalesced: a caller
/// arriving while one is running asks for one more pass instead of queuing its own, so a
/// burst of patches collapses. The pass at the end re-reads the project, so the last
/// thing compiled is always a state observed after the last edit.
pub async fn reload(state: &Arc<AppState>) -> anyhow::Result<Reloaded> {
    let count = || state.preview.halves.each_ref().map(|h| h.loads.load(Ordering::SeqCst));
    let before = count();
    state.preview.pending.store(true, Ordering::SeqCst);
    let _guard = state.preview.compiling.lock().await;
    let mut bakes = (0, Vec::new());
    while state.preview.pending.swap(false, Ordering::SeqCst) {
        bakes = compile_and_load(state).await?;
    }
    let after = count();
    let [video, audio] = [0, 1].map(|i| after[i] != before[i]);
    let path = state.config.cache_dir().join(format!("{}.mlt", state.preview.halves[0].name));
    let (baked, baking) = bakes;
    Ok(Reloaded { path, changed: video || audio, video, audio, baked, baking })
}

/// [`reload`], handing the engine both graphs whether or not they changed: what a person
/// asks for with the Reload button, when something the document does not name — a media
/// file replaced on disk — has changed under the engine.
pub async fn reload_forced(state: &Arc<AppState>) -> anyhow::Result<Reloaded> {
    for half in &state.preview.halves {
        *lock(&half.loaded) = None;
    }
    lock(&state.preview.behind).tried.clear();
    lock(&state.preview.making).clear();
    reload(state).await
}

/// Compile the open sequence (at the preview scale) as two graphs, the picture and the
/// sound, and hand the engine each one it is not already holding exactly.
/// The overlays of the sequences it nests appear baked, from the cache — the missing ones
/// are left out and baked behind (`bake_behind`); its own are left unbaked, and therefore
/// skipped, for the UI's live DOM. The sound needs no bakes at all.
async fn compile_and_load(state: &Arc<AppState>) -> anyhow::Result<(usize, Vec<String>)> {
    // First, because starting an engine clears what it is holding.
    let engine = state.preview.ensure(state).await?;
    let st = state.clone();
    let fresh = tokio::task::spawn_blocking(move || {
        let project = read(&st.project).clone();
        let scale = *lock(&st.preview.scale);
        let even = |v: f64| ((v.round() as u32).max(2)) & !1;
        let (w, h) = (even(project.width as f64 * scale), even(project.height as f64 * scale));
        // The engine loads one compiled file, so editing a sub-sequence means previewing
        // that sub-sequence: it is compiled as the root, exactly as a render of it would be.
        let sequence = st.current_sequence();
        let mut opts = crate::jobs::compile_options(&st);
        opts.size = Some((w, h));
        opts.sequence = Some(sequence.clone());
        // ½ decodes proxies, full the media: one setting for how big and how exact.
        let proxies = (scale < 1.0).then(|| crate::proxies::scan(&st, &project));
        opts.proxies = proxies.as_ref().map(|p| p.ready.clone());
        st.preview.compiles.fetch_add(1, Ordering::SeqCst);
        // Each graph is written, and later sent, only when it differs from the one held.
        let mut fresh = Vec::new();
        let mut report = (0, Vec::new());
        for (i, half) in st.preview.halves.iter().enumerate() {
            opts.streams = half.stream;
            opts.bakes = if half.stream.video() {
                let (bakes, missing) = crate::jobs::cached_nested(&st, &project, &sequence);
                report = (bakes.len(), missing);
                bakes
            } else {
                Default::default()
            };
            let xml = crate::jobs::compile_xml(&project, &opts)?;
            // The document is the whole input: profile, every producer, and the bake
            // directories — whose names are themselves content hashes, so an overlay edited
            // on disk changes this even though the project document did not.
            let stamp = stamp(&xml);
            tracing::debug!("preview: compiled {}, {} bytes, stamp {stamp:016x}", half.name, xml.len());
            if *lock(&half.loaded) != Some(stamp) {
                fresh.push((i, crate::jobs::write_compiled(&st, &xml, half.name)?, stamp));
            }
        }
        Ok::<_, anyhow::Error>((fresh, report, proxies))
    })
    .await??;
    let (fresh, report, proxies) = fresh;
    // Only once the engine has actually been told, so a failed send does not leave us
    // believing it holds something it never received.
    for (i, path, stamp) in fresh {
        let half = &state.preview.halves[i];
        engine.send(&format!("{} {}", half.cmd, path.display())).await?;
        *lock(&half.loaded) = Some(stamp);
        half.loads.fetch_add(1, Ordering::SeqCst);
    }
    bake_behind(state, &report.1);
    if let Some(proxies) = proxies {
        crate::proxies::start(state, proxies);
    }
    Ok(report)
}

/// Bake the nested overlays a compile found missing, in one background job at a time. Not
/// awaited: opening a sequence whose nested overlays were never baked used to wait on
/// hyperframes for every one of them, which is minutes, with the request hanging.
fn bake_behind(state: &Arc<AppState>, missing: &[String]) {
    let revision = read(&state.project).revision;
    let todo: Vec<String> = {
        let mut b = lock(&state.preview.behind);
        if b.revision != revision {
            b.revision = revision;
            b.tried.clear();
        }
        let todo: Vec<String> = missing.iter().filter(|id| !b.tried.contains(*id)).cloned().collect();
        // One job at a time; the reload its end causes finds whatever it did not cover.
        if todo.is_empty() || b.running {
            return;
        }
        b.running = true;
        b.tried.extend(todo.iter().cloned());
        todo
    };
    let st = state.clone();
    crate::jobs::spawn(state.clone(), "preview-bake", move |progress| {
        let project = read(&st.project).clone();
        let (out, errs) = crate::jobs::bake_ids(&st, &project, &todo, progress);
        lock(&st.preview.behind).running = false;
        errs.iter().for_each(|e| tracing::warn!("preview: {e:#}"));
        match errs.is_empty() {
            true => Ok(serde_json::json!({ "baked": out.len() })),
            false => Err(anyhow::anyhow!("{} baked, {} would not: {}", out.len(), errs.len(), errs.iter().map(|e| format!("{e:#}")).collect::<Vec<_>>().join("; "))),
        }
    });
}

fn stamp(xml: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    xml.hash(&mut h);
    h.finish()
}
