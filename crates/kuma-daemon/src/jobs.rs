//! Long-running work: overlay bakes, final renders, frame sampling. Each job runs on a
//! blocking thread and reports through the event bus.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kuma_core::template::Region;
use kuma_core::{ClipSource, Project};
use kuma_mlt::{compile, CompileOptions, OverlayBake, RenderJob, UnbakedPolicy};
use kuma_overlay::{BakeRequest, Format};
use serde::{Deserialize, Serialize};

use crate::state::{AppState, Event};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running { percent: u8, message: String },
    Done { result: serde_json::Value },
    Failed { error: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct JobInfo {
    pub id: u64,
    pub kind: String,
    pub status: JobStatus,
}

#[derive(Default)]
pub struct Jobs {
    next: AtomicU64,
    pub table: Mutex<HashMap<u64, JobInfo>>,
}

impl Jobs {
    pub fn get(&self, id: u64) -> Option<JobInfo> {
        self.table.lock().unwrap().get(&id).cloned()
    }
    pub fn list(&self) -> Vec<JobInfo> {
        let mut v: Vec<_> = self.table.lock().unwrap().values().cloned().collect();
        v.sort_by_key(|j| j.id);
        v
    }
}

/// Spawn a blocking job. `f` receives a progress callback and returns a JSON result.
pub fn spawn<F>(state: Arc<AppState>, kind: &str, f: F) -> u64
where
    F: FnOnce(&dyn Fn(u8, &str)) -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    let id = state.jobs.next.fetch_add(1, Ordering::SeqCst) + 1;
    let kind = kind.to_string();
    state.jobs.table.lock().unwrap().insert(id, JobInfo { id, kind: kind.clone(), status: JobStatus::Running { percent: 0, message: "queued".into() } });
    let st = state.clone();
    let k = kind.clone();
    tokio::task::spawn_blocking(move || {
        let progress = |percent: u8, message: &str| {
            if let Some(j) = st.jobs.table.lock().unwrap().get_mut(&id) {
                j.status = JobStatus::Running { percent, message: message.to_string() };
            }
            let _ = st.events.send(Event::JobProgress { id, kind: k.clone(), percent, message: message.to_string() });
        };
        let outcome = f(&progress);
        let mut table = st.jobs.table.lock().unwrap();
        match outcome {
            Ok(result) => {
                table.insert(id, JobInfo { id, kind: k.clone(), status: JobStatus::Done { result: result.clone() } });
                let _ = st.events.send(Event::JobDone { id, kind: k.clone(), ok: true, message: "done".into(), result });
            }
            Err(e) => {
                let msg = format!("{e:#}");
                table.insert(id, JobInfo { id, kind: k.clone(), status: JobStatus::Failed { error: msg.clone() } });
                let _ = st.events.send(Event::JobDone { id, kind: k.clone(), ok: false, message: msg, result: serde_json::Value::Null });
            }
        }
    });
    id
}

/// Bake every overlay clip in the project (cached). Returns bakes keyed by clip id.
pub fn bake_all(
    state: &AppState,
    project: &Project,
    only: Option<&[String]>,
    format: Format,
    supersample: u32,
    progress: &dyn Fn(u8, &str),
) -> anyhow::Result<HashMap<String, OverlayBake>> {
    let templates = state.templates.read().unwrap().clone();
    let clips: Vec<_> = project
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter(|c| matches!(c.src, ClipSource::Overlay { .. }))
        .filter(|c| only.map(|o| o.iter().any(|id| id == &c.id)).unwrap_or(true))
        .collect();
    let mut out = HashMap::new();
    let n = clips.len().max(1);
    for (i, clip) in clips.iter().enumerate() {
        let ClipSource::Overlay { overlay, vars } = &clip.src else { continue };
        let (dir, manifest) = templates.get(overlay).ok_or_else(|| anyhow::anyhow!("unknown template `{overlay}` for clip `{}`", clip.id))?;
        progress(((i * 100) / n) as u8, &format!("baking {} ({overlay})", clip.id));
        let req = BakeRequest {
            template_dir: dir,
            manifest,
            vars,
            fps: project.fps,
            width: project.width,
            height: project.height,
            duration: clip.duration(),
            speed: clip.speed,
            format,
            supersample,
        };
        let r = state.baker.bake(&req).map_err(|e| anyhow::anyhow!("bake `{}`: {e}", clip.id))?;
        out.insert(clip.id.clone(), r.overlay_bake());
    }
    Ok(out)
}

/// Compile the current project to `.kuma/<name>.mlt`, baking overlays unless `bakes` is given.
pub fn compile_project(state: &AppState, project: &Project, bakes: HashMap<String, OverlayBake>, name: &str, unbaked: UnbakedPolicy, size: Option<(u32, u32)>) -> anyhow::Result<PathBuf> {
    let root = state.config.project_root();
    let mut opts = CompileOptions::new(&root);
    opts.cache_dir = state.config.cache_dir().join("cache");
    opts.bakes = bakes;
    opts.unbaked = unbaked;
    opts.size = size;
    let xml = compile(project, &opts)?;
    let path = state.config.cache_dir().join(format!("{name}.mlt"));
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, xml)?;
    Ok(path)
}

#[derive(Debug, Clone, Deserialize)]
pub struct RenderRequest {
    #[serde(default = "default_preset")]
    pub preset: String,
    pub output: Option<PathBuf>,
    /// Seconds; whole timeline when absent.
    pub range: Option<(f64, f64)>,
    #[serde(default = "one")]
    pub supersample: u32,
}
fn default_preset() -> String {
    "youtube-1080p".into()
}
fn one() -> u32 {
    1
}

pub fn render(state: &AppState, req: &RenderRequest, progress: &dyn Fn(u8, &str)) -> anyhow::Result<serde_json::Value> {
    let project = state.project.read().unwrap().clone();
    let preset = kuma_mlt::presets::find(&req.preset).ok_or_else(|| anyhow::anyhow!("unknown preset `{}`", req.preset))?;
    let bakes = bake_all(state, &project, None, Format::PngSequence, req.supersample, &|p, m| progress(p / 2, m))?;
    let mlt = compile_project(state, &project, bakes, "render", UnbakedPolicy::Error, None)?;
    let output = req.output.clone().unwrap_or_else(|| {
        let name = if project.name.is_empty() { "kuma" } else { project.name.as_str() };
        state.config.project_root().join("renders").join(format!("{name}-{}.{}", preset.id, preset.extension))
    });
    let output = if output.is_absolute() { output } else { state.config.project_root().join(output) };
    let range = req.range.map(|(a, b)| {
        let f = project.fps.clip_frames(a, b);
        (f.in_, f.out)
    });
    let job = RenderJob { mlt_file: mlt.clone(), output: output.clone(), preset, range, extra: vec![] };
    progress(50, "rendering with melt");
    kuma_mlt::render(&job, |p| progress(50 + p.percent / 2, &format!("frame {}", p.frame)))?;
    Ok(serde_json::json!({ "output": output, "mlt": mlt, "preset": preset.id }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct SampleRequest {
    pub t0: f64,
    #[serde(default)]
    pub t1: Option<f64>,
    #[serde(default = "one_usize")]
    pub count: usize,
    /// Crop rectangle in composition pixels.
    #[serde(default)]
    pub rect: Option<Region>,
    /// Output width (keeps aspect). Defaults to 960 for full frames, native for crops.
    #[serde(default)]
    pub width: Option<u32>,
    /// Include baked overlays (default true; slower on first call while bakes are created).
    #[serde(default = "yes")]
    pub overlays: bool,
}
fn one_usize() -> usize {
    1
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub t: f64,
    pub frame: i64,
    pub path: PathBuf,
}

/// Render exact frames of the composite through MLT, optionally cropped.
pub fn sample_frames(state: &AppState, req: &SampleRequest, progress: &dyn Fn(u8, &str)) -> anyhow::Result<Vec<Sample>> {
    let project = state.project.read().unwrap().clone();
    let t1 = req.t1.unwrap_or(req.t0);
    let count = req.count.clamp(1, 32);
    let bakes = if req.overlays {
        // Only bake overlays that intersect the sampled range.
        let ids: Vec<String> = project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| matches!(c.src, ClipSource::Overlay { .. }) && c.end > req.t0 && c.start <= t1)
            .map(|c| c.id.clone())
            .collect();
        bake_all(state, &project, Some(&ids), Format::PngSequence, 1, &|p, m| progress(p / 2, m))?
    } else {
        HashMap::new()
    };
    let mlt = compile_project(state, &project, bakes, "sample", UnbakedPolicy::Skip, None)?;
    let out_dir = state.config.cache_dir().join("samples");
    std::fs::create_dir_all(&out_dir)?;
    let mut samples = Vec::new();
    for i in 0..count {
        let t = if count == 1 { req.t0 } else { req.t0 + (t1 - req.t0) * i as f64 / (count - 1) as f64 };
        let frame = project.fps.frame(t);
        progress((50 + 50 * i / count) as u8, &format!("frame {frame}"));
        let stamp = format!("{}_{}", project.revision, frame);
        let full = out_dir.join(format!("f{stamp}.png"));
        if !full.is_file() {
            kuma_mlt::melt::snapshot(&mlt, frame, &full, None)?;
        }
        let path = if req.rect.is_some() || req.width.is_some() {
            let mut vf = Vec::new();
            if let Some(r) = req.rect {
                vf.push(format!("crop={}:{}:{}:{}", r.w, r.h, r.x, r.y));
            }
            if let Some(w) = req.width {
                vf.push(format!("scale={w}:-2"));
            }
            let name = format!("f{stamp}_{}.png", hash_str(&vf.join(",")));
            let p = out_dir.join(name);
            if !p.is_file() {
                kuma_overlay::media::ffmpeg(&["-i".into(), full.to_string_lossy().into_owned(), "-vf".into(), vf.join(","), p.to_string_lossy().into_owned()])
                    .map_err(|e| anyhow::anyhow!(e))?;
            }
            p
        } else {
            full
        };
        samples.push(Sample { t, frame, path });
    }
    Ok(samples)
}

fn hash_str(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:x}", h.finish())
}

/// Resolve a user-supplied path against the project root.
pub fn resolve_path(state: &AppState, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        state.config.project_root().join(p)
    }
}
