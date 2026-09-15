//! Work: overlay bakes, renders, frame sampling, asset registration. Long-running work
//! runs as a job on a blocking thread and reports through the event bus.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kuma_core::template::Region;
use kuma_core::{BinSource, Project};
use kuma_mlt::{compile, CompileOptions, OverlayBake, RenderJob, UnbakedPolicy};
use kuma_overlay::BakeRequest;
use serde::{Deserialize, Serialize};

use crate::state::{lock, read, AppState, Event};

/// A background job as clients see it: progress while running, then `done` with either
/// a `result` or an `error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub kind: String,
    pub percent: u8,
    pub message: String,
    pub done: bool,
    pub ok: bool,
    #[serde(default)]
    pub result: serde_json::Value,
}

#[derive(Default)]
pub struct Jobs {
    next: AtomicU64,
    table: Mutex<HashMap<u64, Job>>,
}

impl Jobs {
    pub fn get(&self, id: u64) -> Option<Job> {
        lock(&self.table).get(&id).cloned()
    }
}

/// Spawn a blocking job. `f` receives a progress callback and returns a JSON result.
pub fn spawn<F>(state: Arc<AppState>, kind: &str, f: F) -> u64
where
    F: FnOnce(&dyn Fn(u8, &str)) -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    let id = state.jobs.next.fetch_add(1, Ordering::SeqCst) + 1;
    let job = Job { id, kind: kind.to_string(), percent: 0, message: "queued".into(), done: false, ok: false, result: serde_json::Value::Null };
    {
        let mut table = lock(&state.jobs.table);
        // Keep the table bounded: drop the oldest finished jobs beyond a hundred.
        if table.len() >= 100 {
            let mut done: Vec<u64> = table.iter().filter(|(_, j)| j.done).map(|(k, _)| *k).collect();
            done.sort_unstable();
            for k in done.into_iter().take(table.len() + 1 - 100) {
                table.remove(&k);
            }
        }
        table.insert(id, job.clone());
    }
    tokio::task::spawn_blocking(move || {
        let publish = |job: Job| {
            lock(&state.jobs.table).insert(id, job.clone());
            let _ = state.events.send(Event::Job(job));
        };
        let progress = |percent: u8, message: &str| publish(Job { percent, message: message.to_string(), ..job.clone() });
        publish(match f(&progress) {
            Ok(result) => Job { percent: 100, message: "done".into(), done: true, ok: true, result, ..job.clone() },
            Err(e) => Job { message: format!("{e:#}"), done: true, ok: false, ..job.clone() },
        });
    });
    id
}

/// Probe a media file and add it to the bin (shared by the API and the CLI). The id
/// defaults to a slug of the file stem; the stored path is project-relative when possible.
pub fn add_media(state: &AppState, path: &Path, id: Option<String>, base_revision: Option<u64>, origin: &str) -> anyhow::Result<(String, u64, Vec<kuma_core::Issue>)> {
    let abs = resolve_path(state, path);
    if !abs.is_file() {
        anyhow::bail!("file not found: {}", abs.display());
    }
    let probe = kuma_overlay::probe(&abs).map_err(|e| anyhow::anyhow!(e))?;
    let rel = abs.strip_prefix(state.config.project_root()).unwrap_or(&abs);
    let stem = abs.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("media".into());
    let id = id.unwrap_or_else(|| stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect());
    // JSON Patch `add` would silently replace an existing member, rebinding every clip.
    if read(&state.project).bin.contains_key(&id) {
        anyhow::bail!("bin id `{id}` already exists (pass a different id)");
    }
    let item = kuma_core::BinItem {
        name: stem,
        source: kuma_core::BinSource::File { path: rel.to_string_lossy().into_owned(), kind: probe.kind, proxy: None },
        meta: Some(probe.meta),
    };
    let pointer = format!("/bin/{}", id.replace('~', "~0").replace('/', "~1"));
    let patch = vec![json_patch::PatchOperation::Add(json_patch::AddOperation { path: pointer.parse()?, value: serde_json::to_value(item)? })];
    let (rev, warnings) = state.apply_patch(&patch, base_revision, origin)?;
    Ok((id, rev, warnings))
}

/// Bake every overlay clip in the project (cached). Returns bakes keyed by clip id.
///
/// Walks every sequence, not just the master: an overlay two levels down is still in the
/// render, and clip ids are unique across the project so one flat map still addresses it.
pub fn bake_all(state: &AppState, project: &Project, only: Option<&[String]>, progress: &dyn Fn(u8, &str)) -> anyhow::Result<HashMap<String, OverlayBake>> {
    let templates = read(&state.templates).loaded.clone();
    let clips: Vec<_> = project
        .all_clips()
        .map(|(_, _, c)| c)
        .filter(|c| project.item(c).is_some_and(kuma_core::BinItem::is_overlay))
        .filter(|c| only.map(|o| o.iter().any(|id| id == &c.id)).unwrap_or(true))
        .collect();
    let mut out = HashMap::new();
    let n = clips.len().max(1);
    for (i, clip) in clips.iter().enumerate() {
        let Some(BinSource::Overlay { template: overlay, vars }) = project.item(clip).map(|i| &i.source) else { continue };
        let (dir, manifest) = templates.get(overlay).ok_or_else(|| anyhow::anyhow!("unknown template `{overlay}` for clip `{}`", clip.id))?;
        progress(((i * 100) / n) as u8, &format!("baking {} ({overlay})", clip.id));
        let req = BakeRequest { template_dir: dir, manifest, vars, fps: project.fps, width: project.width, height: project.height, duration: clip.duration(), speed: clip.speed };
        let r = state.baker.bake(&req).map_err(|e| anyhow::anyhow!("bake `{}`: {e}", clip.id))?;
        out.insert(clip.id.clone(), r.overlay_bake());
    }
    Ok(out)
}

/// Compile a project to MLT XML with the given bakes. `sequence` is the timeline to
/// compile as the root; `None` means the master, which is what a render always wants.
pub fn compile_xml(
    state: &AppState,
    project: &Project,
    bakes: HashMap<String, OverlayBake>,
    unbaked: UnbakedPolicy,
    size: Option<(u32, u32)>,
    sequence: Option<String>,
) -> anyhow::Result<String> {
    let mut opts = CompileOptions::new(state.config.project_root());
    opts.cache_dir = state.config.cache_dir().join("cache");
    opts.bakes = bakes;
    opts.unbaked = unbaked;
    opts.size = size;
    opts.sequence = sequence;
    Ok(compile(project, &opts)?)
}

/// Compile to `.kuma/<name>.mlt`, replacing the file atomically: a melt or engine that
/// is still reading the previous compile keeps its own (unlinked) copy.
pub fn compile_project(
    state: &AppState,
    project: &Project,
    bakes: HashMap<String, OverlayBake>,
    name: &str,
    unbaked: UnbakedPolicy,
    size: Option<(u32, u32)>,
    sequence: Option<String>,
) -> anyhow::Result<PathBuf> {
    let xml = compile_xml(state, project, bakes, unbaked, size, sequence)?;
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
    /// Sequence to render. The master when absent — rendering a part is the exception.
    #[serde(default)]
    pub sequence: Option<String>,
}
fn default_preset() -> String {
    "youtube-1080p".into()
}

pub fn render(state: &AppState, req: &RenderRequest, progress: &dyn Fn(u8, &str)) -> anyhow::Result<serde_json::Value> {
    let project = read(&state.project).clone();
    let preset = kuma_mlt::presets::find(&req.preset).ok_or_else(|| anyhow::anyhow!("unknown preset `{}`", req.preset))?;
    let sequence = req.sequence.clone().unwrap_or_else(|| project.master.clone());
    if project.seq(&sequence).is_none() {
        anyhow::bail!("`{sequence}` is not a sequence in this project");
    }
    let bakes = bake_all(state, &project, None, &|p, m| progress(p / 2, m))?;
    let mlt = compile_project(state, &project, bakes, "render", UnbakedPolicy::Error, None, Some(sequence.clone()))?;
    let output = req.output.clone().unwrap_or_else(|| {
        let name = if project.name.is_empty() { "kuma" } else { project.name.as_str() };
        let part = if sequence == project.master { String::new() } else { format!("-{sequence}") };
        state.config.project_root().join("renders").join(format!("{name}{part}-{}.{}", preset.id, preset.extension))
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
    Ok(serde_json::json!({ "output": output, "mlt": mlt, "preset": preset.id, "sequence": sequence }))
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
    /// Sequence to sample. The master when absent.
    #[serde(default)]
    pub sequence: Option<String>,
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

/// Render exact frames of the composite through MLT, optionally cropped. Frames are
/// cached per revision, frame and bake set; every cached file is written atomically so
/// two concurrent samples of the same frame never read a half-written PNG.
pub fn sample_frames(state: &AppState, req: &SampleRequest, progress: &dyn Fn(u8, &str)) -> anyhow::Result<Vec<Sample>> {
    let project = read(&state.project).clone();
    let sequence = req.sequence.clone().unwrap_or_else(|| project.master.clone());
    if project.seq(&sequence).is_none() {
        anyhow::bail!("`{sequence}` is not a sequence in this project");
    }
    let t1 = req.t1.unwrap_or(req.t0);
    let count = req.count.clamp(1, 32);
    let bakes = if req.overlays {
        // Skip overlays outside the sampled range — but only on the sequence being
        // sampled, where clip times are the times asked for. A nested sequence's clips are
        // in its own time, and one sequence can be placed more than once, so there is no
        // single window to test them against: those are baked whatever the range.
        let ids: Vec<String> = project
            .all_clips()
            .filter(|(seq, _, c)| {
                project.item(c).is_some_and(kuma_core::BinItem::is_overlay) && (**seq != sequence || (c.end > req.t0 && c.start <= t1))
            })
            .map(|(_, _, c)| c.id.clone())
            .collect();
        bake_all(state, &project, Some(&ids), &|p, m| progress(p / 2, m))?
    } else {
        HashMap::new()
    };
    let out_dir = state.config.cache_dir().join("samples");
    std::fs::create_dir_all(&out_dir)?;
    // Overlay templates can change without a project revision (the agent edits the HTML),
    // so the bake keys are part of the frame cache key.
    let bake_stamp = hash_str(&bakes.values().map(|b| b.resource.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n"));
    let xml = compile_xml(state, &project, bakes, UnbakedPolicy::Skip, None, Some(sequence.clone()))?;
    let mlt_file = tempfile::Builder::new().prefix("sample-").suffix(".mlt").tempfile_in(state.config.cache_dir())?;
    std::fs::write(mlt_file.path(), xml)?;
    // Produce `target` through `f(tmp)` and publish it with a rename.
    let atomic = |target: &Path, f: &dyn Fn(&Path) -> anyhow::Result<()>| -> anyhow::Result<()> {
        if target.is_file() {
            return Ok(());
        }
        let tmp = tempfile::Builder::new().suffix(".png").tempfile_in(&out_dir)?;
        f(tmp.path())?;
        tmp.persist(target)?;
        Ok(())
    };
    let mut samples = Vec::new();
    for i in 0..count {
        let t = if count == 1 { req.t0 } else { req.t0 + (t1 - req.t0) * i as f64 / (count - 1) as f64 };
        let frame = project.fps.frame(t);
        progress((50 + 50 * i / count) as u8, &format!("frame {frame}"));
        let stamp = format!("{}_{sequence}_{}{}", project.revision, frame, if req.overlays { format!("o{bake_stamp}") } else { String::new() });
        let full = out_dir.join(format!("f{stamp}.png"));
        atomic(&full, &|tmp| Ok(kuma_mlt::melt::snapshot(mlt_file.path(), frame, tmp)?))?;
        let path = if req.rect.is_some() || req.width.is_some() {
            let mut vf = Vec::new();
            if let Some(r) = req.rect {
                vf.push(format!("crop={}:{}:{}:{}", r.w, r.h, r.x, r.y));
            }
            if let Some(w) = req.width {
                vf.push(format!("scale={w}:-2"));
            }
            let p = out_dir.join(format!("f{stamp}_{}.png", hash_str(&vf.join(","))));
            atomic(&p, &|tmp| {
                kuma_overlay::media::ffmpeg(&["-i".into(), full.to_string_lossy().into_owned(), "-vf".into(), vf.join(","), tmp.to_string_lossy().into_owned()]).map_err(|e| anyhow::anyhow!(e))
            })?;
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
