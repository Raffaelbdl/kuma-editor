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

/// Which overlays a compile carries bakes for. The three cases are exclusive, which is
/// the point: `Nested` and the live DOM the editor draws partition the overlays between
/// them, so none is ever drawn twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(from = "OverlayScopeRepr")]
pub enum OverlayScope {
    /// Footage only.
    None,
    /// Only the overlays inside the sequences the root *nests*. What a preview of that
    /// root wants: it cannot drive those as live DOM, because their clip times are their
    /// own sequence's rather than the playhead's.
    Nested,
    /// Every overlay the root reaches, its own included — a render, or a sample the agent
    /// is going to look at.
    #[default]
    All,
}

/// Wire form: the `overlays` field has always been a bool, and stays one for clients.
/// `nested` is spellable but is for the daemon's own callers (the preview and the snapshot
/// that stands in for it), which is why the MCP tool still documents a boolean.
#[derive(Deserialize)]
#[serde(untagged, rename_all = "snake_case")]
enum OverlayScopeRepr {
    Toggle(bool),
    Named(OverlayScopeName),
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum OverlayScopeName {
    None,
    Nested,
    All,
}

impl From<OverlayScopeRepr> for OverlayScope {
    fn from(r: OverlayScopeRepr) -> Self {
        match r {
            OverlayScopeRepr::Toggle(true) => OverlayScope::All,
            OverlayScopeRepr::Toggle(false) => OverlayScope::None,
            OverlayScopeRepr::Named(OverlayScopeName::None) => OverlayScope::None,
            OverlayScopeRepr::Named(OverlayScopeName::Nested) => OverlayScope::Nested,
            OverlayScopeRepr::Named(OverlayScopeName::All) => OverlayScope::All,
        }
    }
}

/// Overlay clips in the sequences `root` reaches, on visible tracks. `nested_only` drops
/// `root`'s own, leaving exactly the ones a preview of it cannot drive as live DOM.
///
/// Reachability matters: a bin full of unplaced sequences must cost neither a bake on
/// every preview reload nor a headless browser run on every render.
fn overlay_clips(project: &Project, root: &str, nested_only: bool) -> Vec<String> {
    project
        .compile_order(root)
        .unwrap_or_default()
        .iter()
        .filter(|id| !(nested_only && id.as_str() == root))
        .filter_map(|id| project.seq(id))
        .flat_map(|s| s.tracks.iter().filter(|t| !t.hidden).flat_map(|t| t.clips.iter()))
        .filter(|c| project.item(c).is_some_and(kuma_core::BinItem::is_overlay))
        .map(|c| c.id.clone())
        .collect()
}

/// Bake the overlays `scope` asks for. `Nested` is best effort: a template that will not
/// bake — or no hyperframes at all — costs you that overlay in the preview and never the
/// preview itself, which used to be free because overlays were skipped entirely.
pub fn bake_for(state: &AppState, project: &Project, root: &str, scope: OverlayScope, progress: &dyn Fn(u8, &str)) -> anyhow::Result<HashMap<String, OverlayBake>> {
    match scope {
        OverlayScope::None => Ok(HashMap::new()),
        OverlayScope::Nested => Ok(bake_clips(state, project, &overlay_clips(project, root, true), progress, |e| tracing::warn!("preview: {e:#}"))),
        OverlayScope::All => strict(|on_error| bake_clips(state, project, &overlay_clips(project, root, false), progress, on_error)),
    }
}

/// Bake the named overlay clips, or every one in the project when none are named. This is
/// the explicit `bake` command, where "all" means the whole bin rather than what some
/// timeline reaches — warming the cache for an entry you have not placed yet is the point.
pub fn bake_all(state: &AppState, project: &Project, only: Option<&[String]>, progress: &dyn Fn(u8, &str)) -> anyhow::Result<HashMap<String, OverlayBake>> {
    let ids: Vec<String> = project
        .all_clips()
        .filter(|(_, _, c)| project.item(c).is_some_and(kuma_core::BinItem::is_overlay))
        .map(|(_, _, c)| c.id.clone())
        .filter(|id| only.is_none_or(|o| o.contains(id)))
        .collect();
    strict(|on_error| bake_clips(state, project, &ids, progress, on_error))
}

/// Run a bake that must not lose an overlay: the first failure becomes the result.
fn strict(
    bake: impl FnOnce(&mut dyn FnMut(anyhow::Error)) -> HashMap<String, OverlayBake>,
) -> anyhow::Result<HashMap<String, OverlayBake>> {
    let mut failure = None;
    let out = bake(&mut |e| {
        failure.get_or_insert(e);
    });
    failure.map_or(Ok(out), Err)
}

/// Bake the named overlay clips (cached), handing each failure to `on_error`. Whether a
/// failure is fatal is the caller's to decide — the loop itself does not care.
fn bake_clips(
    state: &AppState,
    project: &Project,
    ids: &[String],
    progress: &dyn Fn(u8, &str),
    mut on_error: impl FnMut(anyhow::Error),
) -> HashMap<String, OverlayBake> {
    let templates = read(&state.templates).loaded.clone();
    let mut out = HashMap::new();
    let n = ids.len().max(1);
    for (i, id) in ids.iter().enumerate() {
        let Some((_, _, clip)) = project.find_clip(id) else { continue };
        let Some(BinSource::Overlay { template: overlay, vars }) = project.item(clip).map(|i| &i.source) else { continue };
        let Some((dir, manifest)) = templates.get(overlay) else {
            on_error(anyhow::anyhow!("unknown template `{overlay}` for clip `{id}`"));
            continue;
        };
        progress(((i * 100) / n) as u8, &format!("baking {id} ({overlay})"));
        let req = BakeRequest { template_dir: dir, manifest, vars, fps: project.fps, width: project.width, height: project.height, duration: clip.duration(), speed: clip.speed };
        match state.baker.bake(&req) {
            Ok(r) => {
                out.insert(id.clone(), r.overlay_bake());
            }
            Err(e) => on_error(anyhow::anyhow!("bake `{id}`: {e}")),
        }
    }
    out
}

/// Compile options pointed at this project's directories. Callers set what they need on
/// it (`bakes`, `unbaked`, `size`, `sequence`) — a new compile knob is then a field rather
/// than another parameter on every function between here and the caller.
pub fn compile_options(state: &AppState) -> CompileOptions {
    let mut opts = CompileOptions::new(state.config.project_root());
    opts.cache_dir = state.config.cache_dir().join("cache");
    opts
}

/// Compile a project to MLT XML.
pub fn compile_xml(project: &Project, opts: &CompileOptions) -> anyhow::Result<String> {
    Ok(compile(project, opts)?)
}

/// Compile to `.kuma/<name>.mlt`, replacing the file atomically: a melt or engine that
/// is still reading the previous compile keeps its own (unlinked) copy.
pub fn compile_project(state: &AppState, project: &Project, opts: &CompileOptions, name: &str) -> anyhow::Result<PathBuf> {
    let xml = compile_xml(project, opts)?;
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
    let sequence = project.sequence_or_master(req.sequence.as_deref()).map_err(|e| anyhow::anyhow!(e))?.clone();
    let mut opts = compile_options(state);
    opts.bakes = bake_for(state, &project, &sequence, OverlayScope::All, &|p, m| progress(p / 2, m))?;
    opts.unbaked = UnbakedPolicy::Error;
    opts.sequence = Some(sequence.clone());
    let mlt = compile_project(state, &project, &opts, "render")?;
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
    /// Which overlays to include. Accepts `true`/`false` on the wire as it always has;
    /// the preview's `nested` scope is for callers inside the daemon.
    #[serde(default)]
    pub overlays: OverlayScope,
    /// Sequence to sample. The master when absent.
    #[serde(default)]
    pub sequence: Option<String>,
}
fn one_usize() -> usize {
    1
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
    let sequence = project.sequence_or_master(req.sequence.as_deref()).map_err(|e| anyhow::anyhow!(e))?.clone();
    let t1 = req.t1.unwrap_or(req.t0);
    let count = req.count.clamp(1, 32);
    let bakes = bake_for(state, &project, &sequence, req.overlays, &|p, m| progress(p / 2, m))?;
    let out_dir = state.config.cache_dir().join("samples");
    std::fs::create_dir_all(&out_dir)?;
    let mut opts = compile_options(state);
    opts.bakes = bakes;
    opts.sequence = Some(sequence.clone());
    // Overlay templates can change without a project revision (the agent edits the HTML),
    // so the bake keys are part of the frame cache key.
    let bake_stamp = hash_str(&opts.bakes.values().map(|b| b.resource.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n"));
    let xml = compile_xml(&project, &opts)?;
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
        let stamp = format!("{}_{sequence}_{}{}", project.revision, frame, if opts.bakes.is_empty() { String::new() } else { format!("o{bake_stamp}") });
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seq(tracks: serde_json::Value) -> serde_json::Value {
        json!({ "source": { "tracks": tracks } })
    }

    fn overlay_track(id: &str, hidden: bool, clips: serde_json::Value) -> serde_json::Value {
        json!({ "id": id, "kind": "overlay", "hidden": hidden, "clips": clips })
    }

    fn clip(id: &str, bin: &str) -> serde_json::Value {
        json!({ "id": id, "start": 0, "end": 4, "bin": bin })
    }

    /// The preview bakes the overlays it cannot drive and nothing else. Its own are live
    /// DOM (baking them too would draw every overlay twice); a hidden track is not in the
    /// picture; and a sequence sitting unplaced in the bin must cost neither a bake on
    /// every reload nor a browser run on every render, just for being there.
    #[test]
    fn a_preview_bakes_the_overlays_it_nests_and_no_others() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "tag": { "source": { "template": "location-tag" } },
                "inner": seq(json!([
                    overlay_track("io1", false, json!([clip("o_nested", "tag")])),
                    overlay_track("io2", true, json!([clip("o_hidden", "tag")]))
                ])),
                "unplaced": seq(json!([overlay_track("uo1", false, json!([clip("o_unplaced", "tag")]))])),
                "main": seq(json!([
                    overlay_track("mo1", false, json!([clip("o_own", "tag")])),
                    json!({ "id": "v1", "clips": [ clip("s1", "inner") ] })
                ]))
            }
        }))
        .unwrap();
        assert_eq!(overlay_clips(&p, "main", true), vec!["o_nested"]);
        // Open the inner sequence and the same overlay becomes live DOM instead.
        assert!(overlay_clips(&p, "inner", true).is_empty());
        // A render of the master wants its own overlay as well — but still nothing from
        // the sequence nobody placed, which would be a headless browser run for nothing.
        // Innermost first, the order `compile_order` walks in.
        assert_eq!(overlay_clips(&p, "main", false), vec!["o_nested", "o_own"]);
    }
}
