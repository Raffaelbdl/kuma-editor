//! The bake driver: overlay folder + vars → hyperframes → RGBA PNG frames (cached).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use kuma_core::manifest::OverlayManifest;

use crate::fonts::{FontLibrary, Origin, Resolved};
use crate::inject::{Contract, FontCss};
use kuma_core::Fps;
use kuma_mlt::OverlayBake;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Generated into a blueprint's folder by `thumbnail()` and `preview()`; never part of a
/// bake, and never copied with a blueprint into a project. They must stay out of the
/// content hash as well as the staging: an overlay whose
/// hash changed when its own preview was generated would invalidate every bake of it, and
/// the preview is itself a bake.
const GENERATED: &[&str] = &["thumb.png", "preview.webp"];
/// The only entry file hyperframes renders.
const ENTRY: &str = "index.html";
/// Where a staged overlay finds the fonts its `font` vars name. kuma writes both the rules
/// and the files, so nothing an author writes points here; the preview serves the same
/// files from its own route.
const STAGED_FONTS: &str = "_kuma/fonts/";
/// Frame rate of the library previews (`thumb.png` is their frame at t = 1 s).
const PREVIEW_FPS: i64 = 12;

#[derive(Debug, Clone)]
pub struct BakeRequest<'a> {
    pub overlay_dir: &'a Path,
    pub manifest: &'a OverlayManifest,
    /// Variable overrides (defaults are applied from the manifest).
    pub vars: &'a Map<String, Value>,
    pub fps: Fps,
    pub width: u32,
    pub height: u32,
    /// Clip duration on the timeline, seconds.
    pub duration: f64,
    /// Playback speed; applied as a time remap of the seek (never by frame duplication).
    pub speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BakeResult {
    pub key: String,
    pub dir: PathBuf,
    /// `frame_%06d.png` pattern, or the single frame of a still.
    pub resource: PathBuf,
    pub frames: i64,
    pub is_still: bool,
    pub cached: bool,
}

impl BakeResult {
    pub fn overlay_bake(&self) -> OverlayBake {
        OverlayBake { resource: self.resource.clone(), is_still: self.is_still }
    }
}

/// An overlay staged for the renderer: where it went and the timing it carries.
#[derive(Debug, Clone)]
pub struct Staged {
    pub dir: PathBuf,
    pub entry: PathBuf,
    pub is_still: bool,
    /// |speed|, which the effective frame rate implements.
    pub speed: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum BakeError {
    #[error("hyperframes not found: {0}")]
    NoRenderer(String),
    #[error("overlay entry file `{0}` not found")]
    NoEntry(PathBuf),
    #[error("invalid vars: {0:?}")]
    Vars(Vec<String>),
    #[error("hyperframes failed: {0}")]
    Render(String),
    #[error("bake produced no frames in {0}")]
    NoFrames(PathBuf),
    #[error("renderer produced {got} frames but {want} were needed — the composition's duration did not take effect")]
    ShortRender { want: i64, got: i64 },
    #[error("unsupported speed {0}: effective fps must stay within 1..240")]
    Speed(f64),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct Baker {
    /// Root of the cache; each bake lives in `<cache>/<key>/`.
    pub cache_dir: PathBuf,
    /// Path to the `hyperframes` executable (or a wrapper script).
    pub hyperframes: PathBuf,
    /// Chrome/Chromium headless shell path, if not left to hyperframes.
    pub browser: Option<PathBuf>,
    /// Installed hyperframes version (part of the cache key); `None` if it cannot run.
    pub version: Option<String>,
    /// Shared `_vendor/` library used when an overlay has none of its own.
    pub vendor_dir: PathBuf,
    /// What `font` vars resolve against. Empty unless the caller sets it.
    pub fonts: FontLibrary,
    /// How many renders may run at once, across every caller of this baker and its clones
    /// (`KUMA_BAKE_JOBS`). Each is its own one-worker hyperframes process: separate processes
    /// were measured byte-identical to a serial bake, where one with several workers was not.
    /// Each costs about 250 MB (Chrome).
    pub concurrency: usize,
    /// Keys being baked by this process right now. A second bake of one waits for the
    /// first and reads its result, rather than rendering the same frames alongside it.
    inflight: Arc<(Mutex<HashSet<String>>, Condvar)>,
    /// The renders running and the queue for more, first come first served.
    slots: Arc<(Mutex<Slots>, Condvar)>,
}

#[derive(Debug, Default)]
struct Slots {
    running: usize,
    /// Tickets handed out, and the next one allowed in when a slot is free.
    issued: u64,
    next: u64,
}

impl Baker {
    /// Locate hyperframes and a browser, preferring the repo's own `node_modules`.
    pub fn discover(repo_root: &Path, vendor_dir: PathBuf, cache_dir: PathBuf) -> Self {
        let hyperframes = std::env::var_os("KUMA_HYPERFRAMES")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| {
                let p = repo_root.join("node_modules/.bin/hyperframes");
                p.is_file().then_some(p)
            })
            .unwrap_or_else(|| PathBuf::from("hyperframes"));
        let browser = std::env::var_os("HYPERFRAMES_BROWSER_PATH")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("PUPPETEER_EXECUTABLE_PATH").map(PathBuf::from))
            .or_else(find_playwright_headless_shell);
        let mut b = Baker {
            cache_dir,
            hyperframes,
            browser,
            version: None,
            vendor_dir,
            fonts: FontLibrary::default(),
            concurrency: kuma_mlt::melt::threads_from_env("KUMA_BAKE_JOBS"),
            inflight: Arc::default(),
            slots: Arc::default(),
        };
        b.version = b.available().ok();
        b
    }

    /// The `_vendor/` folder an overlay resolves `_vendor/...` against: its own, else its
    /// sibling, else the shared library. The preview route serves exactly this folder.
    pub fn vendor_for(&self, overlay_dir: &Path) -> Option<PathBuf> {
        [overlay_dir.join("_vendor"), overlay_dir.parent()?.join("_vendor"), self.vendor_dir.clone()].into_iter().find(|p| p.is_dir())
    }

    /// Resolve `font` vars against these folders (lowest precedence first, each labelled).
    pub fn with_fonts(mut self, dirs: Vec<(PathBuf, Origin)>) -> Self {
        self.fonts = FontLibrary::new(dirs);
        self
    }

    /// What an overlay's `font` vars come to with these (resolved) vars. A family that is
    /// not installed is left out — the overlay's own fallback stack draws it — and listed
    /// as missing, which `lint` reports. The one place fonts are resolved: the bake, the
    /// lint and the live preview all start here.
    pub fn fonts(&self, manifest: &OverlayManifest, vars: &Map<String, Value>) -> Resolved {
        let wanted: Vec<(String, String)> = manifest
            .vars
            .iter()
            .filter(|(_, s)| s.ty == kuma_core::VarType::Font)
            .map(|(k, _)| (k.clone(), vars.get(k).and_then(Value::as_str).unwrap_or_default().to_string()))
            .collect();
        if wanted.is_empty() {
            return Resolved::default();
        }
        self.fonts.resolve(wanted)
    }

    pub fn available(&self) -> Result<String, BakeError> {
        let out = Command::new(&self.hyperframes)
            .arg("--version")
            .output()
            .map_err(|e| BakeError::NoRenderer(format!("{}: {e}", self.hyperframes.display())))?;
        if !out.status.success() {
            return Err(BakeError::NoRenderer(String::from_utf8_lossy(&out.stderr).into_owned()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Cache key for a request: everything that changes the pixels (overlay files, the
    /// vendor library, resolved vars, renderer and browser builds, fps, size, duration
    /// and speed — the last two ignored for stills) and nothing else.
    pub fn key(&self, req: &BakeRequest) -> Result<String, BakeError> {
        self.key_with(&self.folder_digest(req.overlay_dir)?, req)
    }

    /// The costly, per-folder part of the key: its files, its vendor library, the renderer.
    pub fn folder_digest(&self, overlay_dir: &Path) -> Result<String, BakeError> {
        let mut h = Sha256::new();
        h.update(b"kuma-bake-v5\n");
        if let Some(v) = &self.version {
            h.update(v.as_bytes());
        }
        if let Some(b) = &self.browser {
            let meta = std::fs::metadata(b).ok();
            h.update(format!("{} {:?} {:?}", b.display(), meta.as_ref().map(|m| m.len()), meta.and_then(|m| m.modified().ok())).as_bytes());
        }
        h.update(hash_dir(overlay_dir)?.as_bytes());
        if let Some(v) = self.vendor_for(overlay_dir) {
            h.update(hash_dir(&v)?.as_bytes());
        }
        Ok(hex::encode(h.finalize()))
    }

    /// [`key`](Self::key) given the folder's [`folder_digest`](Self::folder_digest).
    pub fn key_with(&self, folder_digest: &str, req: &BakeRequest) -> Result<String, BakeError> {
        let mut h = Sha256::new();
        h.update(folder_digest.as_bytes());
        let vars = req.manifest.resolve_vars(req.vars);
        h.update(serde_json::to_vec(&vars).unwrap_or_default());
        let fonts = self.fonts(req.manifest, &vars);
        // A var the contract now writes differently (an unclosed quote, escaped) is a
        // different document from the one an older kuma baked; no other var is.
        h.update(crate::inject::repaired_css(&vars, &fonts.vars));
        // The fonts drawn, by content: the same family name can be different files on
        // another machine, or after a font is replaced.
        for family in fonts.families {
            h.update(family.name.as_bytes());
            for face in &family.faces {
                h.update(format!("\0{:?} {} {}\0", face.weight, face.italic, face.digest).as_bytes());
            }
        }
        let (duration, speed) = if req.manifest.is_static { (0.0, 0.0) } else { (req.duration, req.speed) };
        h.update(format!("\n{}/{} {}x{} d={duration:.6} s={speed:.6}", req.fps.num, req.fps.den, req.width, req.height).as_bytes());
        Ok(hex::encode(h.finalize())[..24].to_string())
    }

    /// The bake already in the cache for `req`, rendering nothing: `None` when there is none.
    pub fn cached(&self, req: &BakeRequest) -> Result<Option<BakeResult>, BakeError> {
        self.cached_keyed(req, None)
    }

    /// [`cached`](Self::cached), given `req`'s [`key`](Self::key) when the caller has it.
    pub fn cached_keyed(&self, req: &BakeRequest, key: Option<&str>) -> Result<Option<BakeResult>, BakeError> {
        let key = match key {
            Some(k) => k.to_string(),
            None => self.key(req)?,
        };
        Ok(read_done(&self.cache_dir.join(key)))
    }

    /// Bake (or fetch from cache). Work happens in a private directory that is renamed
    /// into place at the end, so concurrent bakes of the same key never see each other's
    /// partial output and a crash leaves no half-written cache entry.
    pub fn bake(&self, req: &BakeRequest) -> Result<BakeResult, BakeError> {
        self.bake_keyed(req, None)
    }

    /// [`bake`](Self::bake), given `req`'s [`key`](Self::key) when the caller has it, so
    /// the folder is not hashed again.
    pub fn bake_keyed(&self, req: &BakeRequest, key: Option<&str>) -> Result<BakeResult, BakeError> {
        let problems = req.manifest.check_vars(req.vars);
        if !problems.is_empty() {
            return Err(BakeError::Vars(problems));
        }
        let key = match key {
            Some(k) => k.to_string(),
            None => self.key(req)?,
        };
        let dir = self.cache_dir.join(&key);
        if let Some(r) = read_done(&dir) {
            return Ok(r);
        }
        let _claim = self.claim(&key);
        // Whoever held the key may have finished it while this one waited.
        if let Some(r) = read_done(&dir) {
            return Ok(r);
        }
        let _slot = self.slot();
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let work = WorkDir(self.cache_dir.join(format!("{key}.tmp-{}-{nanos:x}", std::process::id())));
        std::fs::create_dir_all(&work.0)?;
        let result = self.bake_into(req, &work.0, &dir)?;
        // The marker goes in before the rename, so what lands under `dir` is complete.
        std::fs::write(work.0.join("done.json"), serde_json::to_vec_pretty(&result).unwrap())?;
        if std::fs::rename(&work.0, &dir).is_err() {
            // Someone finished the same key first; theirs is as good as ours.
            return read_done(&dir).ok_or_else(|| BakeError::Other(format!("cache entry {} is incomplete", dir.display())));
        }
        std::mem::forget(work);
        Ok(result)
    }

    /// Hold `key` until the returned claim drops, waiting while another bake holds it.
    fn claim(&self, key: &str) -> Claim<'_> {
        let (keys, freed) = &*self.inflight;
        let mut held = keys.lock().unwrap_or_else(PoisonError::into_inner);
        while held.contains(key) {
            held = freed.wait(held).unwrap_or_else(PoisonError::into_inner);
        }
        held.insert(key.to_string());
        Claim(&self.inflight, key.to_string())
    }

    /// Wait for one of the `concurrency` render slots, in the order asked: a preview's one
    /// bake queues behind at most the renders already running, never behind a whole batch
    /// that asks for slot after slot. Held until the returned slot drops.
    fn slot(&self) -> Slot<'_> {
        let (slots, freed) = &*self.slots;
        let mut s = slots.lock().unwrap_or_else(PoisonError::into_inner);
        let ticket = s.issued;
        s.issued += 1;
        while s.next != ticket || s.running >= self.concurrency.max(1) {
            s = freed.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
        s.next += 1;
        s.running += 1;
        // The next ticket may fit in a slot that is still free.
        freed.notify_all();
        Slot(&self.slots)
    }

    /// Stage an overlay into `src`: its own files, the shared `_vendor/` when it has none
    /// of its own, and `index.html` with the contract injected — byte for byte what the
    /// renderer opens. The bake renders this; the lint reads it, so what is checked is
    /// what would run rather than the source with the injection imagined on top.
    pub fn stage(&self, req: &BakeRequest, src: &Path) -> Result<Staged, BakeError> {
        // Shared libraries live in `_vendor/` and are referenced by overlays as
        // `_vendor/...`; an overlay's own copy (already staged with it) wins, exactly as
        // the preview serves it.
        copy_dir(req.overlay_dir, src)?;
        if let Some(vendor) = self.vendor_for(req.overlay_dir).filter(|v| !v.starts_with(req.overlay_dir)) {
            copy_dir(&vendor, &src.join("_vendor"))?;
        }
        let vars = req.manifest.resolve_vars(req.vars);
        let is_still = req.manifest.is_static;
        // A still is one frame at native speed; otherwise the effective frame rate
        // implements speed: frame k must show internal time k·s/fps.
        let speed = if is_still { 1.0 } else { req.speed.abs() };
        let internal_duration = if is_still { 1.0 / req.fps.as_f64() } else { req.duration * speed };
        let entry = src.join(ENTRY);
        if !entry.is_file() {
            return Err(BakeError::NoEntry(entry));
        }
        let html = std::fs::read_to_string(&entry)?;
        // The fonts go where the rules say.
        let fonts = self.fonts(req.manifest, &vars);
        for family in &fonts.families {
            let dir = src.join(STAGED_FONTS).join(family.slug());
            std::fs::create_dir_all(&dir)?;
            for (file, face) in family.files() {
                std::fs::copy(&face.path, dir.join(file))?;
            }
        }
        let faces = fonts.css(STAGED_FONTS);
        let fonts = FontCss { faces: &faces, vars: &fonts.vars };
        let contract = Contract { vars: &vars, fps: req.fps, width: req.width, height: req.height, duration: internal_duration, speed: req.speed, bake: true, clip: None, head_extra: "", fonts };
        std::fs::write(&entry, contract.apply(&html))?;
        Ok(Staged { dir: src.to_path_buf(), entry, is_still, speed })
    }

    /// Render into `work`; the result describes paths under `dir`, where `work` ends up.
    fn bake_into(&self, req: &BakeRequest, work: &Path, dir: &Path) -> Result<BakeResult, BakeError> {
        // 1. Stage the overlay with kuma's injection script.
        let staged = self.stage(req, &work.join("src"))?;
        let (src, is_still, speed) = (staged.dir, staged.is_still, staged.speed);
        // The renderer takes the same resolved vars the staged document carries.
        let vars = req.manifest.resolve_vars(req.vars);
        let eff_fps = effective_fps(req.fps, speed).ok_or(BakeError::Speed(req.speed))?;

        // 2. Render. One worker: multi-worker capture was measured to be non-deterministic.
        let raw_dir = work.join("raw");
        let mut cmd = Command::new(&self.hyperframes);
        cmd.arg("render")
            .arg(&src)
            .args(["--format", "png-sequence", "-o"])
            .arg(&raw_dir)
            .args(["--workers", "1", "--quiet", "--no-best-effort", "--no-browser-gpu", "--fps", &eff_fps])
            .arg("--variables")
            .arg(Value::Object(vars.clone()).to_string())
            .env("HYPERFRAMES_TELEMETRY", "0");
        if let Some(b) = &self.browser {
            cmd.env("HYPERFRAMES_BROWSER_PATH", b);
        }
        let out = cmd.output().map_err(|e| BakeError::NoRenderer(format!("{}: {e}", self.hyperframes.display())))?;
        if !out.status.success() {
            let mut msg = String::from_utf8_lossy(&out.stderr).into_owned();
            msg.push_str(&String::from_utf8_lossy(&out.stdout));
            let tail: String = msg.chars().rev().take(4000).collect::<Vec<_>>().into_iter().rev().collect();
            return Err(BakeError::Render(tail));
        }

        // 3. Number the frames, padding by at most two frames of rounding; anything more
        //    means the renderer ignored the duration we asked for, and padding would
        //    silently freeze the overlay for the rest of the clip.
        let frames_dir = work.join("frames");
        std::fs::create_dir_all(&frames_dir)?;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&raw_dir)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "png")).collect();
        files.sort();
        if files.is_empty() {
            return Err(BakeError::NoFrames(raw_dir));
        }
        if req.speed < 0.0 && !is_still {
            files.reverse();
        }
        let want = if is_still { 1 } else { req.fps.length(req.duration) };
        let got = (files.len() as i64).min(want);
        for (i, f) in files.iter().take(got as usize).enumerate() {
            std::fs::rename(f, frames_dir.join(frame_name(i as i64 + 1)))?;
        }
        if want - got > 2 {
            return Err(BakeError::ShortRender { want, got });
        }
        for n in got + 1..=want {
            std::fs::copy(frames_dir.join(frame_name(got)), frames_dir.join(frame_name(n)))?;
        }
        let _ = std::fs::remove_dir_all(&raw_dir);
        Ok(BakeResult {
            key: dir.file_name().unwrap().to_string_lossy().into_owned(),
            dir: dir.to_path_buf(),
            resource: dir.join("frames").join(if is_still { frame_name(1) } else { "frame_%06d.png".into() }),
            frames: want,
            is_still,
            cached: false,
        })
    }

    /// Render a still of `overlay_dir` with `vars` to `out` — a 480px-wide frame about 1 s
    /// in, after most entrance animations have settled. Used both for a blueprint's own
    /// `thumb.png` (manifest defaults) and for a bin entry's thumbnail, which shows that
    /// entry's actual configuration.
    pub fn thumbnail(&self, overlay_dir: &Path, manifest: &OverlayManifest, vars: &Map<String, Value>, out: &Path) -> Result<(), BakeError> {
        // The same bake the moving preview is cut from, so the two cost one render and
        // share a cache entry; the still is its frame at t = 1 s, after most entrances
        // have settled.
        let r = self.bake(&Self::preview_request(overlay_dir, manifest, vars))?;
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let at_one_second = (PREVIEW_FPS + 1).min(r.frames);
        std::fs::copy(r.dir.join("frames").join(frame_name(at_one_second)), out)?;
        Ok(())
    }

    /// The one bake both previews are cut from: 12 fps, 480×270, the first seconds. Enough
    /// to see the entrance and the first beat of the loop; the exit is anchored to a
    /// length this preview does not have, so showing it would be a lie anyway. A still
    /// overlay renders one frame of it.
    pub fn preview_request<'a>(overlay_dir: &'a Path, manifest: &'a OverlayManifest, vars: &'a Map<String, Value>) -> BakeRequest<'a> {
        BakeRequest {
            overlay_dir,
            manifest,
            vars,
            fps: Fps::new(PREVIEW_FPS as u32, 1),
            width: 480,
            height: 270,
            duration: manifest.duration_default.min(3.0).max(1.0),
            speed: 1.0,
        }
    }

    /// Bake a **moving** preview of an overlay with some vars: an animated WebP with alpha,
    /// written to `out`.
    ///
    /// A still cannot show what an overlay is. Every one of these is an animation, and the
    /// browser and the bin were both picking one frozen frame out of it — usually the
    /// settled pose, which is the least informative moment. This is the first seconds,
    /// looping, at a size a card can hold.
    ///
    /// WebP because it is the only format that is animated, alpha-capable and understood
    /// by every browser; the alpha matters because the preview sits on the editor's own
    /// background, not on black. A still overlay renders one frame and the result is a
    /// one-frame WebP, which is simply a picture.
    pub fn preview(&self, overlay_dir: &Path, manifest: &OverlayManifest, vars: &Map<String, Value>, out: &Path) -> Result<(), BakeError> {
        let r = self.bake(&Self::preview_request(overlay_dir, manifest, vars))?;
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let frames = r.dir.join("frames").join("frame_%06d.png");
        let tmp = out.with_extension("webp.tmp-");
        // `-loop 0` is the animation's own loop, forever; `yuva420p` keeps the alpha. No
        // frame-rate mode flag: `-fps_mode` only exists from ffmpeg 5.1, Ubuntu 22.04
        // ships 4.4, and an image sequence with `-framerate` needs none.
        let args: Vec<String> = [
            "-framerate", &PREVIEW_FPS.to_string(), "-i", &frames.to_string_lossy(),
            "-vcodec", "libwebp", "-lossless", "0", "-q:v", "70", "-compression_level", "4",
            "-loop", "0", "-pix_fmt", "yuva420p", "-an",
            // Named, because the file is written under a `.tmp` suffix and ffmpeg would
            // otherwise have to guess the format from an extension that is not there.
            "-f", "webp",
            &tmp.to_string_lossy(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        crate::media::ffmpeg(&args).map_err(|e| BakeError::Other(format!("encoding the preview: {e}")))?;
        // Published with a rename so a reader never sees a half-written file, the way the
        // bake cache publishes its own directory.
        std::fs::rename(&tmp, out)?;
        Ok(())
    }
}

fn frame_name(n: i64) -> String {
    format!("frame_{n:06}.png")
}

/// Scratch still being written into `.kuma/`: every writer names it `….tmp-…` until renamed.
pub fn is_in_flight(name: &str) -> bool {
    name.contains(".tmp-")
}

/// A bake's private directory: removed on drop unless it was renamed into the cache.
struct WorkDir(PathBuf);

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A finished cache entry, if `dir` holds one. Paths are rebuilt from `dir`, so a moved
/// or copied project keeps a valid cache.
/// A key this process is baking; dropping it, however the bake ended, wakes the waiters.
struct Claim<'a>(&'a (Mutex<HashSet<String>>, Condvar), String);

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let (keys, freed) = self.0;
        keys.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.1);
        freed.notify_all();
    }
}

/// A render slot; dropping it, however the bake ended, lets the next ticket in.
struct Slot<'a>(&'a (Mutex<Slots>, Condvar));

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let (slots, freed) = self.0;
        slots.lock().unwrap_or_else(PoisonError::into_inner).running -= 1;
        freed.notify_all();
    }
}

fn read_done(dir: &Path) -> Option<BakeResult> {
    let mut r: BakeResult = serde_json::from_slice(&std::fs::read(dir.join("done.json")).ok()?).ok()?;
    r.resource = dir.join("frames").join(r.resource.file_name()?);
    r.dir = dir.to_path_buf();
    r.cached = true;
    Some(r)
}

/// `fps / speed` as an ffmpeg-style rational string, or None if out of hyperframes' range.
fn effective_fps(fps: Fps, speed: f64) -> Option<String> {
    let (sn, sd) = to_rational(speed);
    let mut num = fps.num as u64 * sd;
    let mut den = fps.den as u64 * sn;
    let g = gcd(num, den);
    num /= g;
    den /= g;
    let f = num as f64 / den as f64;
    if !(1.0..=240.0).contains(&f) {
        return None;
    }
    Some(if den == 1 { num.to_string() } else { format!("{num}/{den}") })
}

fn to_rational(x: f64) -> (u64, u64) {
    let den = 1000u64;
    let num = (x * den as f64).round().max(1.0) as u64;
    let g = gcd(num, den);
    (num / g, den / g)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Files that are neither staged nor hashed: dotfiles and anything kuma generated into
/// the overlay folder itself.
fn ignored(name: &std::ffi::OsStr) -> bool {
    let n = name.to_string_lossy();
    n.starts_with('.') || GENERATED.contains(&n.as_ref())
}

/// Copy an overlay folder, leaving out dotfiles and what is generated from it (thumbnails,
/// previews) at every depth. What the bake stages, and what a blueprint copy carries.
pub fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let p = entry.path();
        let name = entry.file_name();
        if ignored(&name) {
            continue;
        }
        if p.is_dir() {
            copy_dir(&p, &dst.join(name))?;
        } else {
            std::fs::copy(&p, dst.join(name))?;
        }
    }
    Ok(())
}

/// Content hash of every file in a directory (sorted, path + bytes).
fn hash_dir(dir: &Path) -> std::io::Result<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let p = entry?.path();
            if p.file_name().map(ignored).unwrap_or(false) {
                continue;
            }
            if p.is_dir() {
                walk(&p, base, out)?;
            } else {
                out.push(p.strip_prefix(base).unwrap_or(&p).to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for rel in files {
        h.update(rel.to_string_lossy().as_bytes());
        h.update(b"\0");
        h.update(std::fs::read(dir.join(&rel))?);
        h.update(b"\0");
    }
    Ok(hex::encode(h.finalize()))
}

/// Playwright's cache holds `chromium_headless_shell-<build>/chrome-linux/headless_shell`;
/// take the newest build.
fn find_playwright_headless_shell() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").map(PathBuf::from).into_iter().collect();
    roots.push(PathBuf::from("/opt/pw-browsers"));
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".cache/ms-playwright"));
    }
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        let mut candidates: Vec<(u64, PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let build = name.strip_prefix("chromium_headless_shell-")?.parse().ok()?;
                let shell = e.path().join("chrome-linux/headless_shell");
                shell.is_file().then_some((build, shell))
            })
            .collect();
        candidates.sort();
        if let Some((_, p)) = candidates.pop() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copied_folder_leaves_its_generated_previews_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("fonts")).unwrap();
        for f in ["overlay.json", "index.html", "thumb.png", "preview.webp", "fonts/a.woff2", ".DS_Store"] {
            std::fs::write(src.join(f), b"x").unwrap();
        }
        let dst = tmp.path().join("dst");
        copy_dir(&src, &dst).unwrap();
        assert!(dst.join("overlay.json").is_file() && dst.join("index.html").is_file() && dst.join("fonts/a.woff2").is_file());
        assert!(!dst.join("thumb.png").exists() && !dst.join("preview.webp").exists() && !dst.join(".DS_Store").exists());
    }


    #[test]
    fn effective_fps_implements_speed() {
        assert_eq!(effective_fps(Fps::new(30, 1), 1.0).unwrap(), "30");
        assert_eq!(effective_fps(Fps::new(30, 1), 0.5).unwrap(), "60");
        assert_eq!(effective_fps(Fps::new(30, 1), 2.0).unwrap(), "15");
        assert_eq!(effective_fps(Fps::new(30000, 1001), 1.0).unwrap(), "30000/1001");
        assert_eq!(effective_fps(Fps::new(30, 1), 0.7).unwrap(), "300/7");
        assert!(effective_fps(Fps::new(30, 1), 0.05).is_none());
    }
}
