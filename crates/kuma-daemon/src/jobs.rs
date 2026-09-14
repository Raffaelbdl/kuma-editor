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
    {
        let mut table = state.jobs.table.lock().unwrap();
        // Keep the table bounded: drop the oldest finished jobs beyond a hundred.
        if table.len() >= 100 {
            let mut done: Vec<u64> = table.iter().filter(|(_, j)| !matches!(j.status, JobStatus::Running { .. })).map(|(k, _)| *k).collect();
            done.sort_unstable();
            for k in done.into_iter().take(table.len() + 1 - 100) {
                table.remove(&k);
            }
        }
        table.insert(id, JobInfo { id, kind: kind.clone(), status: JobStatus::Running { percent: 0, message: "queued".into() } });
    }
    tokio::task::spawn_blocking(move || {
        let set = |status: JobStatus| state.jobs.table.lock().unwrap().insert(id, JobInfo { id, kind: kind.clone(), status });
        let progress = |percent: u8, message: &str| {
            set(JobStatus::Running { percent, message: message.to_string() });
            let _ = state.events.send(Event::JobProgress { id, kind: kind.clone(), percent, message: message.to_string() });
        };
        let (status, ok, message, result) = match f(&progress) {
            Ok(result) => (JobStatus::Done { result: result.clone() }, true, "done".to_string(), result),
            Err(e) => {
                let msg = format!("{e:#}");
                (JobStatus::Failed { error: msg.clone() }, false, msg, serde_json::Value::Null)
            }
        };
        set(status);
        let _ = state.events.send(Event::JobDone { id, kind: kind.clone(), ok, message, result });
    });
    id
}

/// Probe and register an asset (shared by the API and the CLI). The id defaults to a
/// slug of the file stem; the stored path is project-relative when possible.
pub fn add_asset(state: &AppState, path: &Path, id: Option<String>, base_revision: Option<u64>, origin: &str) -> anyhow::Result<(String, u64, Vec<kuma_core::Issue>)> {
    let abs = resolve_path(state, path);
    if !abs.is_file() {
        anyhow::bail!("file not found: {}", abs.display());
    }
    let probe = kuma_overlay::probe(&abs).map_err(|e| anyhow::anyhow!(e))?;
    let rel = abs.strip_prefix(state.config.project_root()).unwrap_or(&abs);
    let id = id.unwrap_or_else(|| {
        let stem = abs.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("asset".into());
        stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
    });
    let asset = kuma_core::Asset { path: rel.to_string_lossy().into_owned(), kind: probe.kind, proxy: None, meta: Some(probe.meta) };
    let pointer = format!("/assets/{}", id.replace('~', "~0").replace('/', "~1"));
    let patch = vec![json_patch::PatchOperation::Add(json_patch::AddOperation { path: pointer.parse()?, value: serde_json::to_value(asset)? })];
    let (rev, warnings) = state.apply_patch(&patch, base_revision, origin)?;
    Ok((id, rev, warnings))
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

/// Compile a project to MLT XML with the given bakes.
pub fn compile_xml(state: &AppState, project: &Project, bakes: HashMap<String, OverlayBake>, unbaked: UnbakedPolicy, size: Option<(u32, u32)>) -> anyhow::Result<String> {
    let mut opts = CompileOptions::new(state.config.project_root());
    opts.cache_dir = state.config.cache_dir().join("cache");
    opts.bakes = bakes;
    opts.unbaked = unbaked;
    opts.size = size;
    Ok(compile(project, &opts)?)
}

/// Compile to `.kuma/<name>.mlt`, replacing the file atomically: a melt or engine that
/// is still reading the previous compile keeps its own (unlinked) copy.
pub fn compile_project(state: &AppState, project: &Project, bakes: HashMap<String, OverlayBake>, name: &str, unbaked: UnbakedPolicy, size: Option<(u32, u32)>) -> anyhow::Result<PathBuf> {
    let xml = compile_xml(state, project, bakes, unbaked, size)?;
    let dir = state.config.cache_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.mlt"));
    let tmp = tempfile::Builder::new().prefix(name).suffix(".mlt.tmp").tempfile_in(&dir)?;
    std::fs::write(tmp.path(), xml)?;
    tmp.persist(&path)?;
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
    let extra = vec![("ar".to_string(), project.sample_rate.to_string())];
    let job = RenderJob { mlt_file: mlt.clone(), output: output.clone(), preset, range, extra };
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
    // Samples run concurrently (agent + UI snapshots), so each gets its own XML file.
    let out_dir = state.config.cache_dir().join("samples");
    std::fs::create_dir_all(&out_dir)?;
    // Overlay templates can change without a project revision (the agent edits the HTML),
    // so the bake keys are part of the frame cache key.
    let bake_stamp = hash_str(&bakes.values().map(|b| b.resource.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n"));
    let xml = compile_xml(state, &project, bakes, UnbakedPolicy::Skip, None)?;
    let mlt_file = tempfile::Builder::new().prefix("sample-").suffix(".mlt").tempfile_in(state.config.cache_dir())?;
    std::fs::write(mlt_file.path(), xml)?;
    let mlt = mlt_file.path();
    let mut samples = Vec::new();
    for i in 0..count {
        let t = if count == 1 { req.t0 } else { req.t0 + (t1 - req.t0) * i as f64 / (count - 1) as f64 };
        let frame = project.fps.frame(t);
        progress((50 + 50 * i / count) as u8, &format!("frame {frame}"));
        // The UI's snapshot (no overlays) and the agent's sample (with overlays, whose
        // bakes are hashed in) must never share a cached frame.
        let stamp = format!("{}_{}{}", project.revision, frame, if req.overlays { format!("o{bake_stamp}") } else { String::new() });
        let full = out_dir.join(format!("f{stamp}.png"));
        if !full.is_file() {
            kuma_mlt::melt::snapshot(mlt, frame, &full, None)?;
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
