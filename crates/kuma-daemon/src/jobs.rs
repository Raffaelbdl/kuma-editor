//! Work: overlay bakes, renders, frame sampling, asset registration. Long-running work
//! runs as a job on a blocking thread and reports through the event bus.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kuma_core::template::Region;
use kuma_core::{BinSource, Project};
use kuma_mlt::{compile, CompileOptions, OverlayBake, RenderJob, UnbakedPolicy};
use kuma_overlay::{BakeRequest, TranscribeOptions};
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
/// `parent` lists it under that entry, in the same edit.
pub fn add_media(state: &AppState, path: &Path, id: Option<String>, parent: Option<String>, base_revision: Option<u64>, origin: &str) -> anyhow::Result<(String, u64, Vec<kuma_core::Issue>)> {
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
        parent,
        order: None,
    };
    let (rev, warnings) = state.apply_patch(&[bin_op(&id, "", serde_json::to_value(item)?)?], base_revision, origin)?;
    Ok((id, rev, warnings))
}

/// The template a transcript lands in. Its `cues` var is the transcript; the rest is looks.
const SUBTITLES_TEMPLATE: &str = "subtitles";

#[derive(Debug, Clone, Deserialize)]
pub struct TranscribeRequest {
    /// The media entry to transcribe. The result is listed under it, unplaced.
    #[serde(default)]
    pub bin: Option<String>,
    /// Or a clip playing one: the result is placed over that clip, aligned to it — same
    /// start, end and speed, and the entry's `offset` is the clip's `in` — on the first
    /// overlay track with room there, else a new one. One of `bin` or `clip`.
    #[serde(default)]
    pub clip: Option<String>,
    /// Which entry the transcript is. Naming one is how you say *this* entry: to
    /// `replace` it, or to insist on a name for a new one. Left out, the daemon mints
    /// `<bin>-subs` (or `<bin>-subs-2`…) once the words are in, so a second run adds a
    /// second transcript instead of colliding, and no name can be taken meanwhile.
    #[serde(default)]
    pub id: Option<String>,
    /// Overwrite the transcript that is there: the entry `id` names, or `<bin>-subs`.
    /// Without it, naming an entry that exists is refused rather than overwritten.
    #[serde(default)]
    pub replace: bool,
    #[serde(flatten)]
    pub options: TranscribeOptions,
    #[serde(default = "default_origin")]
    pub origin: String,
}
fn default_origin() -> String {
    "ui".into()
}

/// Transcribe a media entry's audio and add the result to the bin as a `subtitles`
/// overlay entry under that media, cues in source seconds. From a clip, also place it
/// over the clip. With `replace`, an entry that is already there keeps everything but its
/// words. One patch either way, so it is revision-checked and one undo step.
///
/// The model runs for minutes, and the document is edited while it does. So nothing about
/// *where the words go* is decided until they exist: the id, the entry to replace, the
/// track and the clip id are all worked out against the project as it is at the moment
/// the patch is applied, under the edit lock. Deciding any of it up front raced every
/// edit made meanwhile — an unrelated one threw the transcription away, and one that took
/// the id had its entry silently overwritten.
pub fn transcribe(state: &AppState, req: &TranscribeRequest, progress: &dyn Fn(u8, &str)) -> anyhow::Result<serde_json::Value> {
    let project = read(&state.project).clone();
    let (bin, clip) = match (&req.bin, &req.clip) {
        (Some(bin), None) => (bin.clone(), None),
        (None, Some(id)) => {
            let (seq, _, clip) = project.find_clip(id).ok_or_else(|| anyhow::anyhow!("no clip `{id}`"))?;
            (clip.bin.clone(), Some((seq.clone(), clip.id.clone())))
        }
        _ => anyhow::bail!("pass exactly one of `bin` or `clip`"),
    };
    let item = project.bin.get(&bin).ok_or_else(|| anyhow::anyhow!("no bin entry `{bin}`"))?;
    let BinSource::File { path, .. } = &item.source else { anyhow::bail!("`{bin}` is not a media file") };
    if item.meta.as_ref().is_some_and(|m| !m.has_audio) {
        anyhow::bail!("`{bin}` has no audio");
    }
    if !read(&state.templates).loaded.contains_key(SUBTITLES_TEMPLATE) {
        anyhow::bail!("the `{SUBTITLES_TEMPLATE}` template is not in any library; run `kuma library`");
    }
    // A courtesy, not the check that counts: refuse an id that is plainly taken before
    // spending minutes on it. The one that decides runs below, against the live project.
    if let Some(id) = req.id.as_ref().filter(|_| !req.replace) {
        anyhow::ensure!(!project.bin.contains_key(id), "`{id}` already exists; pass `replace` to overwrite its transcript, or a different `id`");
    }

    progress(2, "extracting audio");
    let media = resolve_path(state, Path::new(path));
    // The model's own percentage, over most of the job: it is the part that takes minutes.
    let report = |p: u8| progress(5 + (p as u16 * 80 / 100) as u8, &format!("transcribing {p}%"));
    let cues = state.transcriber.transcribe(&media, &state.config.cache_dir().join("transcripts"), &req.options, &report).map_err(|e| anyhow::anyhow!(e))?;
    progress(90, "adding to the bin");

    let cues_value = serde_json::to_value(&cues)?;
    let (outcome, rev, warnings) = state.apply_built(&req.origin, |p| {
        // Everything below reads `p`, the project as it is now — never the snapshot above.
        let media_item = p.bin.get(&bin).ok_or_else(|| anyhow::anyhow!("`{bin}` is gone"))?;
        // The clip may have been moved, retrimmed or deleted while the model ran; its
        // current state is what the subtitles are aligned to.
        let placement = match &clip {
            None => None,
            Some((_, clip_id)) => {
                let (seq, _, c) = p.find_clip(clip_id).ok_or_else(|| anyhow::anyhow!("clip `{clip_id}` is gone"))?;
                Some((seq.clone(), c.clone()))
            }
        };
        // Where the words go, decided here and nowhere earlier: the entry named, else the
        // one `replace` means, else a free name minted with the words already in hand.
        let target = req.id.clone().or_else(|| req.replace.then(|| format!("{bin}-subs")));
        let (id, replacing) = match target {
            Some(id) => match p.bin.get(&id) {
                Some(existing) => {
                    anyhow::ensure!(req.replace, "`{id}` already exists; pass `replace` to overwrite its transcript, or a different `id`");
                    anyhow::ensure!(existing.is_overlay(), "`{id}` is not an overlay entry");
                    (id, true)
                }
                // Named, or asked to replace, something that is not there: make it.
                None => (id, false),
            },
            None => (p.free_bin_id(&format!("{bin}-subs")), false),
        };

        let mut ops = Vec::new();
        if replacing {
            // Only the vars the transcript owns: the entry's own styling stays as it is.
            ops.push(bin_op(&id, "/source/vars/cues", cues_value.clone())?);
            if let Some((_, c)) = &placement {
                ops.push(bin_op(&id, "/source/vars/offset", c.source_in.into())?);
            }
        } else {
            let mut vars = serde_json::Map::from_iter([("cues".to_string(), cues_value.clone())]);
            if let Some((_, c)) = &placement {
                vars.insert("offset".into(), c.source_in.into());
            }
            let entry = kuma_core::BinItem {
                name: format!("Subtitles: {}", media_item.display_name(&bin)),
                source: BinSource::Overlay { template: SUBTITLES_TEMPLATE.into(), vars },
                meta: None,
                parent: Some(bin.clone()),
                order: None,
            };
            // The entry first: the clip that places it must find it.
            ops.push(bin_op(&id, "", serde_json::to_value(entry)?)?);
        }
        // Place it, unless it is on a timeline already — a re-run must not stack a second
        // clip on the one the first run put there.
        let place = placement.as_ref().filter(|_| !p.all_clips().any(|(_, _, c)| c.bin == id));
        if let Some((seq, c)) = place {
            // The overlay clip copies the media clip's start, end and speed, so at every
            // timeline instant the template's own time is `source − in`: the cues line up.
            let tracks = &p.seq(seq).ok_or_else(|| anyhow::anyhow!("`{seq}` is not a sequence"))?.tracks;
            let free = |t: &kuma_core::Track| t.kind == kuma_core::TrackKind::Overlay && !t.clips.iter().any(|x| x.start < c.end && x.end > c.start);
            let ti = match tracks.iter().position(free) {
                Some(ti) => ti,
                None => {
                    let track = kuma_core::Track { id: p.free_track_or_clip_id("o"), kind: kuma_core::TrackKind::Overlay, name: "Subtitles".into(), muted: false, hidden: false, clips: vec![], transitions: vec![] };
                    ops.push(add_op(&format!("/bin/{seq}/source/tracks/-"), serde_json::to_value(track)?)?);
                    tracks.len()
                }
            };
            let placed = kuma_core::Clip { id: p.free_track_or_clip_id("subs"), start: c.start, end: c.end, bin: id.clone(), source_in: 0.0, speed: c.speed, volume: 1.0, muted: false, effects: vec![], label: String::new(), group: String::new() };
            ops.push(add_op(&format!("/bin/{seq}/source/tracks/{ti}/clips/-"), serde_json::to_value(placed)?)?);
        }
        Ok((ops, (id, replacing, place.is_some())))
    })?;
    let (id, replaced, placed) = outcome;
    Ok(serde_json::json!({ "bin": id, "cues": cues.len(), "replaced": replaced, "placed": placed, "revision": rev, "warnings": warnings }))
}

fn add_op(pointer: &str, value: serde_json::Value) -> anyhow::Result<json_patch::PatchOperation> {
    Ok(json_patch::PatchOperation::Add(json_patch::AddOperation { path: pointer.parse()?, value }))
}

/// `add /bin/<id><rest>`, the id escaped as a pointer segment so a bad one is refused by
/// the validator rather than landing somewhere else.
fn bin_op(id: &str, rest: &str, value: serde_json::Value) -> anyhow::Result<json_patch::PatchOperation> {
    add_op(&format!("/bin/{}{rest}", id.replace('~', "~0").replace('/', "~1")), value)
}

pub type Bakes = HashMap<String, OverlayBake>;

/// Which overlays a compile carries bakes for. `Nested` and the live DOM the editor draws
/// partition the overlays between them, so none is ever drawn twice.
///
/// On the wire this is the bool it always was — `Nested` is for the daemon's own callers
/// (the preview, and the snapshot that stands in for it), not for clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(from = "bool")]
pub enum OverlayScope {
    /// Footage only.
    None,
    /// Only the overlays inside the sequences the root *nests* — the ones a preview of
    /// that root cannot drive as live DOM, their clip times being their own sequence's.
    Nested,
    /// Every overlay the root reaches, its own included.
    #[default]
    All,
}

impl From<bool> for OverlayScope {
    fn from(on: bool) -> Self {
        if on { OverlayScope::All } else { OverlayScope::None }
    }
}

/// Overlay clips in the sequences `root` reaches; `nested_only` drops `root`'s own, and
/// `within` keeps only those overlapping a time window — on `root` alone, where clip times
/// are the times asked for. A nested sequence's clips are in its own time, and one
/// sequence can be placed more than once, so there is no single window to test them
/// against.
///
/// Reachability matters: a bin full of unplaced sequences must cost neither a bake on
/// every preview reload nor a browser run on every render. A *hidden* track does not:
/// the compiler still emits its clips (hiding is per track, on the tractor), so skipping
/// their bakes would make a render fail with "overlay clip has no bake".
fn overlay_clips(project: &Project, root: &str, nested_only: bool, within: Option<(f64, f64)>) -> Vec<String> {
    project
        .compile_order(root)
        .unwrap_or_default()
        .iter()
        .filter(|id| !(nested_only && id.as_str() == root))
        .filter_map(|id| Some((id.as_str() == root, project.seq(id)?)))
        .flat_map(|(is_root, s)| s.clips().map(move |c| (is_root, c)))
        .filter(|(is_root, c)| match within {
            Some((t0, t1)) if *is_root => c.end > t0 && c.start <= t1,
            _ => true,
        })
        .filter(|(_, c)| project.item(c).is_some_and(kuma_core::BinItem::is_overlay))
        .map(|(_, c)| c.id.clone())
        .collect()
}

/// Bake the overlays `scope` asks for. `Nested` is best effort: a template that will not
/// bake — or no hyperframes at all — costs you that overlay in the preview and never the
/// preview itself, which used to be free because overlays were skipped entirely.
pub fn bake_for(
    state: &AppState,
    project: &Project,
    root: &str,
    scope: OverlayScope,
    within: Option<(f64, f64)>,
    progress: &dyn Fn(u8, &str),
) -> anyhow::Result<Bakes> {
    let nested_only = match scope {
        OverlayScope::None => return Ok(Bakes::new()),
        OverlayScope::Nested => true,
        OverlayScope::All => false,
    };
    let (out, errs) = bake_clips(state, project, &overlay_clips(project, root, nested_only, within), progress);
    if !nested_only {
        return strict((out, errs));
    }
    errs.iter().for_each(|e| tracing::warn!("preview: {e:#}"));
    Ok(out)
}

/// Bake the named overlay clips, or every one in the project when none are named. This is
/// the explicit `bake` command, where "all" means the whole bin rather than what some
/// timeline reaches — warming the cache for an entry you have not placed yet is the point.
pub fn bake_all(state: &AppState, project: &Project, only: Option<&[String]>, progress: &dyn Fn(u8, &str)) -> anyhow::Result<Bakes> {
    let ids: Vec<String> = project
        .all_clips()
        .filter(|(_, _, c)| project.item(c).is_some_and(kuma_core::BinItem::is_overlay))
        .map(|(_, _, c)| c.id.clone())
        .filter(|id| only.is_none_or(|o| o.contains(id)))
        .collect();
    strict(bake_clips(state, project, &ids, progress))
}

/// A bake that must not lose an overlay: the first failure becomes the result.
fn strict((out, errs): (Bakes, Vec<anyhow::Error>)) -> anyhow::Result<Bakes> {
    errs.into_iter().next().map_or(Ok(out), Err)
}

/// Bake the named overlay clips (cached), returning what succeeded and what did not.
/// Whether a failure is fatal is the caller's to decide — the loop does not care.
fn bake_clips(state: &AppState, project: &Project, ids: &[String], progress: &dyn Fn(u8, &str)) -> (Bakes, Vec<anyhow::Error>) {
    let templates = read(&state.templates).loaded.clone();
    let (mut out, mut errs) = (Bakes::new(), Vec::new());
    let n = ids.len().max(1);
    for (i, id) in ids.iter().enumerate() {
        let Some((_, _, clip)) = project.find_clip(id) else { continue };
        let Some(BinSource::Overlay { template: overlay, vars }) = project.item(clip).map(|i| &i.source) else { continue };
        let Some((dir, manifest)) = templates.get(overlay) else {
            errs.push(anyhow::anyhow!("unknown template `{overlay}` for clip `{id}`"));
            continue;
        };
        progress(((i * 100) / n) as u8, &format!("baking {id} ({overlay})"));
        let req = BakeRequest { template_dir: dir, manifest, vars, fps: project.fps, width: project.width, height: project.height, duration: clip.duration(), speed: clip.speed };
        match state.baker.bake(&req) {
            Ok(r) => {
                out.insert(id.clone(), r.overlay_bake());
            }
            Err(e) => errs.push(anyhow::anyhow!("bake `{id}`: {e}")),
        }
    }
    (out, errs)
}

/// Lint one template at the project's frame and rate, so a rule that depends on either
/// judges the size the video is actually in. Blocking: it stages a copy and runs a
/// subprocess, so callers on the runtime go through `spawn_blocking`.
pub fn lint_template(state: &AppState, id: &str) -> anyhow::Result<kuma_overlay::LintReport> {
    let project = read(&state.project).clone();
    // Cloned out rather than borrowed: the lock must not be held across the subprocess,
    // or a template rescan waits on a linter.
    let (dir, manifest) = read(&state.templates).loaded.get(id).cloned().ok_or_else(|| anyhow::anyhow!("unknown template `{id}`"))?;
    let req = kuma_overlay::BakeRequest {
        template_dir: &dir,
        manifest: &manifest,
        vars: &serde_json::Map::new(),
        fps: project.fps,
        width: project.width,
        height: project.height,
        duration: manifest.duration_default,
        speed: 1.0,
    };
    Ok(state.baker.lint(&req)?)
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
    write_compiled(state, &compile_xml(project, opts)?, name)
}

/// Publish already-compiled XML as `.kuma/<name>.mlt`, replacing it atomically. Split from
/// `compile_project` because the preview compiles first and only writes when the result
/// differs from what the engine already holds.
pub fn write_compiled(state: &AppState, xml: &str, name: &str) -> anyhow::Result<PathBuf> {
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
    opts.bakes = bake_for(state, &project, &sequence, OverlayScope::All, None, &|p, m| progress(p / 2, m))?;
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
    /// Sample one overlay bin **entry** instead of a timeline: the workbench's view of an
    /// overlay, for an agent. An entry is a whole thing before any clip places it, so
    /// checking an animation must not require putting one in the cut first. Refuses to be
    /// combined with `sequence`.
    #[serde(default)]
    pub overlay: Option<String>,
    /// How long to run that overlay for. The template's own default when absent — an
    /// entry has no length until a clip gives it one.
    #[serde(default)]
    pub duration: Option<f64>,
    /// What to composite it over, as a colour entry would spell it (`#102030`). Black by
    /// default, and `transparent` for straight alpha: an overlay is transparent, and a
    /// frame of it over nothing is not a picture of anything.
    #[serde(default)]
    pub backdrop: Option<String>,
    /// With `overlay`: the frame to render into, when it is not the project's. A template
    /// has to look right at 1920×1080 and 1080×1920 alike, and checking the other way
    /// round should not mean editing the project to find out.
    #[serde(default)]
    pub frame_width: Option<u32>,
    #[serde(default)]
    pub frame_height: Option<u32>,
}
fn one_usize() -> usize {
    1
}

/// One frame of the master at t=0, overlays included. Callers name what they change;
/// adding a field here no longer means visiting every construction.
impl Default for SampleRequest {
    fn default() -> Self {
        SampleRequest {
            t0: 0.0,
            t1: None,
            count: 1,
            rect: None,
            width: None,
            overlays: OverlayScope::default(),
            sequence: None,
            overlay: None,
            duration: None,
            backdrop: None,
            frame_width: None,
            frame_height: None,
        }
    }
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
    let doc = read(&state.project).clone();
    // Sampling an entry is sampling a timeline that holds nothing else. Everything below
    // is then the ordinary path — the same bake, the same compiler, the same melt — so
    // what comes back is the composite the render would produce, not an approximation.
    let (project, sequence, scope) = match req.overlay.as_deref() {
        Some(id) => {
            if req.sequence.is_some() {
                anyhow::bail!("`overlay` samples one bin entry on its own and `sequence` samples a timeline; ask for one or the other");
            }
            let (p, seq) = overlay_project(state, &doc, id, req)?;
            // An overlay sample with the overlays turned off would be a picture of the
            // backdrop, so that combination is read as a mistake rather than obeyed.
            (p, seq, OverlayScope::All)
        }
        None => {
            let seq = doc.sequence_or_master(req.sequence.as_deref()).map_err(|e| anyhow::anyhow!(e))?.clone();
            (doc, seq, req.overlays)
        }
    };
    let t1 = req.t1.unwrap_or(req.t0);
    let count = req.count.clamp(1, 32);
    // Only the overlays this sample can actually show: one frame must not pay for a bake
    // of every overlay on the timeline, nor fail because one somewhere else is broken.
    let bakes = bake_for(state, &project, &sequence, scope, Some((req.t0, t1)), &|p, m| progress(p / 2, m))?;
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

/// An overlay entry as the workbench or an agent asks to see it: at some length, in some
/// frame. Both are views, not document values — an entry has no length until a clip
/// gives it one, and the project's frame is only the default — so they are resolved
/// against the template and the project here, and **clamped rather than validated**:
/// nothing is stored, and a silly number costs one render.
#[derive(Debug, Clone)]
pub struct EntryView {
    pub template: String,
    pub vars: serde_json::Map<String, serde_json::Value>,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
}

/// Why an entry cannot be viewed. Typed rather than a message, because the HTTP layer
/// answers "gone" and "not that kind of thing" with different statuses and must not have
/// to read a sentence to tell them apart.
#[derive(Debug, thiserror::Error)]
pub enum EntryError {
    #[error("no bin entry `{0}`")]
    NoEntry(String),
    #[error("`{0}` is not an overlay entry")]
    NotOverlay(String),
    #[error("unknown template `{0}`")]
    NoTemplate(String),
}

pub fn entry_view(state: &AppState, doc: &Project, id: &str, duration: Option<f64>, width: Option<u32>, height: Option<u32>) -> Result<EntryView, EntryError> {
    let item = doc.bin.get(id).ok_or_else(|| EntryError::NoEntry(id.into()))?;
    let kuma_core::BinSource::Overlay { template, vars } = &item.source else {
        return Err(EntryError::NotOverlay(id.into()));
    };
    let default = {
        let templates = read(&state.templates);
        templates.loaded.get(template).ok_or_else(|| EntryError::NoTemplate(template.clone()))?.1.duration_default
    };
    Ok(EntryView {
        template: template.clone(),
        vars: vars.clone(),
        duration: duration.filter(|d| d.is_finite() && *d > 0.0).map(|d| d.clamp(0.05, 3600.0)).unwrap_or(default),
        width: width.unwrap_or(doc.width).clamp(16, 16384),
        height: height.unwrap_or(doc.height).clamp(16, 16384),
    })
}

/// A throwaway project whose master timeline is one overlay clip over a colour, so an
/// entry can be sampled with no placement in the real document.
///
/// Built rather than patched in: the document must not gain a clip nobody asked for, and
/// undo must not have to know about one. It is never stored and never validated — it is
/// well formed by construction, and it lives for one compile.
///
/// The sequence id carries a hash of what the caller asked for, because it is part of the
/// frame cache's key: two samples of the same entry and frame over different backdrops
/// are different pictures and must not share a cached file.
///
/// Public for the tests: the shape is the contract (one clip, the real fps and revision,
/// the asked-for frame), and it must not need a renderer to be checked.
pub fn overlay_project(state: &AppState, doc: &Project, id: &str, req: &SampleRequest) -> anyhow::Result<(Project, String)> {
    let item = doc.bin.get(id).ok_or_else(|| anyhow::anyhow!("no bin entry `{id}`"))?;
    let view = entry_view(state, doc, id, req.duration, req.frame_width, req.frame_height)?;
    let d = view.duration;
    let colour = match req.backdrop.as_deref() {
        // An overlay is transparent, so "nothing behind it" has to be asked for by name.
        Some("transparent") => "#00000000".to_string(),
        Some(c) => c.to_string(),
        None => "#000000".to_string(),
    };
    let frame = (view.width, view.height);
    let stamp = hash_str(&format!("{d}/{colour}/{}x{}", frame.0, frame.1));
    let seq_id = format!("sample-{id}-{stamp}");
    let bg_id = format!("{seq_id}-bg");
    let clip = |cid: &str, bin: &str| kuma_core::Clip {
        id: cid.into(),
        start: 0.0,
        end: d,
        bin: bin.into(),
        source_in: 0.0,
        speed: 1.0,
        volume: 1.0,
        muted: false,
        effects: Vec::new(),
        label: String::new(),
        group: String::new(),
    };
    let track = |tid: &str, kind: kuma_core::TrackKind, clip: kuma_core::Clip| kuma_core::Track {
        id: tid.into(),
        kind,
        name: String::new(),
        muted: false,
        hidden: false,
        clips: vec![clip],
        transitions: Vec::new(),
    };
    let entry = |source| kuma_core::BinItem { name: String::new(), source, meta: None, parent: None, order: None };
    let sequence = kuma_core::Sequence {
        tracks: vec![
            track(&format!("{seq_id}-v"), kuma_core::TrackKind::Video, clip(&format!("{seq_id}-bgc"), &bg_id)),
            track(&format!("{seq_id}-o"), kuma_core::TrackKind::Overlay, clip(&format!("{seq_id}-oc"), id)),
        ],
        markers: Vec::new(),
    };
    // The rate and the revision are the real project's — an overlay is sampled as the
    // video it is going into would show it, and the revision keeps the frame cache honest
    // when the entry's vars change — and so is the frame unless the caller asked for
    // another one to check the other orientation in.
    let mut out = doc.clone();
    (out.width, out.height) = frame;
    out.bin = [
        (id.to_string(), item.clone()),
        (bg_id, entry(kuma_core::BinSource::Color { color: colour })),
        (seq_id.clone(), entry(kuma_core::BinSource::Sequence(sequence))),
    ]
    .into();
    out.master = seq_id.clone();
    Ok((out, seq_id))
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
    /// DOM (baking them too would draw every overlay twice), and a sequence sitting
    /// unplaced in the bin must cost neither a bake on every reload nor a browser run on
    /// every render, just for being there. A hidden track's overlays *are* baked: the
    /// compiler still emits them, so skipping those made renders fail.
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
        // A hidden track is still compiled — hiding happens per track on the tractor — so
        // its overlays still need bakes, or a render fails with "overlay clip has no bake".
        assert_eq!(overlay_clips(&p, "main", true, None), vec!["o_nested", "o_hidden"]);
        // Open the inner sequence and the same overlays become live DOM instead.
        assert!(overlay_clips(&p, "inner", true, None).is_empty());
        // A render of the master wants its own overlay as well — but still nothing from
        // the sequence nobody placed, which would be a headless browser run for nothing.
        // Innermost first, the order `compile_order` walks in.
        assert_eq!(overlay_clips(&p, "main", false, None), vec!["o_nested", "o_hidden", "o_own"]);
    }

    /// Sampling one frame must not bake every overlay on the timeline. The window applies
    /// to the sequence being sampled, whose clip times are the times asked for; a nested
    /// sequence's are its own, and it may be placed more than once, so those are kept.
    #[test]
    fn a_time_window_narrows_the_root_but_not_what_it_nests() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "tag": { "source": { "template": "location-tag" } },
                "inner": seq(json!([overlay_track("io1", false, json!([clip("o_nested", "tag")]))])),
                "main": seq(json!([
                    overlay_track("mo1", false, json!([
                        { "id": "o_early", "start": 0, "end": 2, "bin": "tag" },
                        { "id": "o_late", "start": 50, "end": 52, "bin": "tag" }
                    ])),
                    json!({ "id": "v1", "clips": [ clip("s1", "inner") ] })
                ]))
            }
        }))
        .unwrap();
        assert_eq!(overlay_clips(&p, "main", false, Some((0.0, 1.0))), vec!["o_nested", "o_early"]);
        assert_eq!(overlay_clips(&p, "main", false, Some((50.5, 51.0))), vec!["o_nested", "o_late"]);
    }
}
