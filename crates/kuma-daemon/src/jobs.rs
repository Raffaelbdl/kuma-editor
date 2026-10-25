//! Work: overlay bakes, renders, frame sampling, asset registration. Long-running work
//! runs as a job on a blocking thread and reports through the event bus.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kuma_core::manifest::Region;
use kuma_core::{BinSource, Issue, Project, Severity};
use kuma_mlt::{compile, CompileOptions, OverlayBake, RenderJob, UnbakedPolicy};
use kuma_overlay::{BakeRequest, TranscribeOptions};
use serde::{Deserialize, Serialize};

use crate::activity::Call;
use crate::state::{lock, read, AppState, Event};

/// Where a job is in its life: what clients read. `message` is only a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// A render waiting its turn.
    #[default]
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// A background job as clients see it: progress while running, then a final `state` with
/// either a `result` or an error in `message`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub kind: String,
    #[serde(default)]
    pub state: JobState,
    pub percent: u8,
    pub message: String,
    #[serde(default)]
    pub result: serde_json::Value,
    /// Whether `POST /api/jobs/{id}/cancel` can stop it.
    #[serde(default)]
    pub cancellable: bool,
    /// What it was asked to do, as the queue shows it: a render's sequence, preset, output.
    #[serde(default)]
    pub params: serde_json::Value,
    /// Who asked for it, as the request said: `null` when it said neither an origin nor a
    /// call (an old client).
    #[serde(default)]
    pub asked_by: Option<AskedBy>,
}

/// Who asked for a job, stamped the way an edit is: its `origin` (`ui: render`,
/// `mcp: sample_frames`, `cli: bake`), the author that reads as
/// ([`crate::history::author`]), and the agent's tool call when one asked. Only an agent's
/// job is followed in the activity feed ([`AskedBy::call`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskedBy {
    /// `user`, `agent` or `other`.
    pub author: String,
    pub origin: String,
    pub tool: Option<String>,
    /// The MCP client's name (`claude-code`), or `agent`, when a tool call asked.
    pub client: Option<String>,
}

impl AskedBy {
    /// From a request's `origin` and `call`: `None` when it gave neither. A call without an
    /// origin is an agent's, `agent: <tool>`.
    pub fn new(origin: Option<String>, call: Option<Call>) -> Option<AskedBy> {
        let origin = origin.filter(|o| !o.trim().is_empty());
        let origin = match (origin, &call) {
            (Some(o), _) => o,
            (None, Some(c)) => format!("agent: {}", c.tool),
            (None, None) => return None,
        };
        let (tool, client) = call.map(|c| (c.tool, c.client)).unzip();
        Some(AskedBy { author: crate::history::author(&origin).to_string(), origin, tool, client })
    }

    pub fn is_agent(&self) -> bool {
        self.author == "agent"
    }

    /// The call a job of `kind` is recorded as in the activity feed — the agent's alone:
    /// its tool, else the job's kind, and its client, else `agent`.
    pub fn call(&self, kind: &str) -> Option<Call> {
        self.is_agent().then(|| Call { tool: self.tool.clone().unwrap_or_else(|| kind.to_string()), client: self.client.clone().unwrap_or_else(|| "agent".into()) })
    }
}

/// A job's request body as a client sends it: the request, with who is asking — its
/// `origin`, as an edit's, and the tool call when an agent's is (`call`, which `kuma mcp`
/// adds beside `origin: "mcp: <tool>"`).
#[derive(Debug, Deserialize)]
pub struct Asked<T> {
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub call: Option<Call>,
    #[serde(flatten)]
    pub req: T,
}

impl<T> Asked<T> {
    /// Who asked, as the job keeps it, and the request.
    pub fn split(self) -> (Option<AskedBy>, T) {
        (AskedBy::new(self.origin, self.call), self.req)
    }
}

impl JobState {
    /// Done, failed or cancelled: it will not change again.
    pub fn is_final(self) -> bool {
        matches!(self, JobState::Done | JobState::Failed | JobState::Cancelled)
    }
}

impl Job {
    fn with_state(self, state: JobState, message: impl Into<String>) -> Job {
        Job { state, message: message.into(), ..self }
    }
}

#[derive(Default)]
pub struct Jobs {
    next: AtomicU64,
    table: Mutex<HashMap<u64, Job>>,
    /// The flags of the cancellable jobs not yet finished.
    cancels: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    lane: Lane,
}

/// Renders run one at a time, first come first served: the front ticket takes the lane.
#[derive(Default)]
struct Lane {
    state: Mutex<LaneState>,
    turn: std::sync::Condvar,
}

#[derive(Default)]
struct LaneState {
    waiting: std::collections::VecDeque<u64>,
    busy: bool,
    next: u64,
}

impl Jobs {
    pub fn get(&self, id: u64) -> Option<Job> {
        lock(&self.table).get(&id).cloned()
    }

    /// Every job still in the table, oldest first, optionally of one kind.
    pub fn list(&self, kind: Option<&str>) -> Vec<Job> {
        let mut jobs: Vec<Job> = lock(&self.table).values().filter(|j| kind.is_none_or(|k| j.kind == k)).cloned().collect();
        jobs.sort_by_key(|j| j.id);
        jobs
    }

    /// Forget every finished job and return their ids. Live ones stay.
    pub fn clear_finished(&self) -> Vec<u64> {
        let mut ids = Vec::new();
        lock(&self.table).retain(|id, j| !j.state.is_final() || {
            ids.push(*id);
            false
        });
        ids
    }

    /// The unfinished renders that will write `output`.
    pub fn writing(&self, output: &Path) -> Vec<u64> {
        let mut ids: Vec<u64> = lock(&self.table)
            .values()
            .filter(|j| j.kind == "render" && !j.state.is_final() && j.params.get("output").and_then(serde_json::Value::as_str) == output.to_str())
            .map(|j| j.id)
            .collect();
        ids.sort_unstable();
        ids
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CancelError {
    #[error("no such job")]
    NotFound,
    #[error("job {0} has already finished")]
    Finished(u64),
    #[error("a `{0}` job cannot be cancelled")]
    NotCancellable(String),
}

/// Ask a job to stop: at once while queued, by killing melt while it encodes. It then
/// reports itself `cancelled`.
pub fn cancel(state: &AppState, id: u64) -> Result<Job, CancelError> {
    let job = state.jobs.get(id).ok_or(CancelError::NotFound)?;
    if job.state.is_final() {
        return Err(CancelError::Finished(id));
    }
    let flag = lock(&state.jobs.cancels).get(&id).cloned().ok_or_else(|| CancelError::NotCancellable(job.kind.clone()))?;
    flag.store(true, Ordering::SeqCst);
    state.jobs.lane.turn.notify_all(); // a queued render sees the flag at once
    Ok(job)
}

/// Spawn a blocking job. `f` receives a progress callback and returns a JSON result.
pub fn spawn<F>(state: Arc<AppState>, kind: &str, f: F) -> u64
where
    F: FnOnce(&dyn Fn(u8, &str)) -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    spawn_with(state, kind, serde_json::Value::Null, None, f)
}

/// [`spawn`], with `params` saying what the job works on, for the queue to show, and who
/// asked for it, if the request said.
pub fn spawn_with<F>(state: Arc<AppState>, kind: &str, params: serde_json::Value, asked_by: Option<AskedBy>, f: F) -> u64
where
    F: FnOnce(&dyn Fn(u8, &str)) -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    spawn_inner(state, kind, params, asked_by, false, move |progress, _| f(progress))
}

/// Spawn a cancellable job: `queued` until its first progress report. `f` watches the
/// flag; once set, the job ends `cancelled` however `f` returns.
pub fn spawn_cancellable<F>(state: Arc<AppState>, kind: &str, params: serde_json::Value, asked_by: Option<AskedBy>, f: F) -> u64
where
    F: FnOnce(&dyn Fn(u8, &str), &AtomicBool) -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    spawn_inner(state, kind, params, asked_by, true, f)
}

/// How often progress reaches clients when only the message moves: melt reports every frame.
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// Every job is created here, and ends here: so the activity record of a job the agent
/// asked for starts and finishes here too, whichever way the job ends.
fn spawn_inner<F>(state: Arc<AppState>, kind: &str, params: serde_json::Value, asked_by: Option<AskedBy>, cancellable: bool, f: F) -> u64
where
    F: FnOnce(&dyn Fn(u8, &str), &AtomicBool) -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    let id = state.jobs.next.fetch_add(1, Ordering::SeqCst) + 1;
    // Only a cancellable job can wait for a turn; anything else starts at once.
    let (start, message) = if cancellable { (JobState::Queued, "queued") } else { (JobState::Running, "starting") };
    let job = Job { id, kind: kind.to_string(), state: start, percent: 0, message: String::new(), result: serde_json::Value::Null, cancellable, params, asked_by }.with_state(start, message);
    let flag = Arc::new(AtomicBool::new(false));
    if cancellable {
        lock(&state.jobs.cancels).insert(id, flag.clone());
    }
    {
        let mut table = lock(&state.jobs.table);
        // Keep the table bounded: drop the oldest finished jobs beyond a hundred.
        if table.len() >= 100 {
            let mut done: Vec<u64> = table.iter().filter(|(_, j)| j.state.is_final()).map(|(k, _)| *k).collect();
            done.sort_unstable();
            for k in done.into_iter().take(table.len() + 1 - 100) {
                table.remove(&k);
            }
        }
        table.insert(id, job.clone());
    }
    // Said at once, or a queued render would be invisible until its turn.
    let _ = state.events.send(Event::Job(job.clone()));
    if let Some(call) = job.asked_by.as_ref().and_then(|a| a.call(kind)) {
        let revision = read(&state.project).revision;
        let record = lock(&state.activity).from_job(&job, &call, revision, crate::history::now_ms());
        let _ = state.events.send(Event::Activity(Box::new(record)));
    }
    tokio::task::spawn_blocking(move || {
        let publish = |job: Job| {
            lock(&state.jobs.table).insert(id, job.clone());
            let _ = state.events.send(Event::Job(job));
        };
        // (when last published, percent, still queued)
        let last = std::cell::Cell::new((std::time::Instant::now(), 0u8, job.state == JobState::Queued));
        let progress = |percent: u8, message: &str| {
            let (at, was, queued) = last.get();
            if queued || percent != was || at.elapsed() >= PROGRESS_EVERY {
                last.set((std::time::Instant::now(), percent, false));
                publish(Job { percent, ..job.clone() }.with_state(JobState::Running, message));
            }
        };
        // A panic is a failure like any other: a job left running would never end, nor
        // would the activity following it.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&progress, &flag))).unwrap_or_else(|_| Err(anyhow::anyhow!("the job panicked")));
        let percent = last.get().1;
        let end = match outcome {
            _ if flag.load(Ordering::SeqCst) => Job { percent, ..job.clone() }.with_state(JobState::Cancelled, "cancelled"),
            Ok(result) => Job { percent: 100, result, ..job.clone() }.with_state(JobState::Done, "done"),
            Err(e) => Job { percent, ..job.clone() }.with_state(JobState::Failed, format!("{e:#}")),
        };
        publish(end.clone());
        if end.asked_by.as_ref().is_some_and(AskedBy::is_agent) {
            if let Some(record) = lock(&state.activity).job_ended(&end, crate::history::now_ms(), &state.config.samples_dir()) {
                let _ = state.events.send(Event::Activity(Box::new(record)));
            }
        }
        // Only now: a cancel just before the final state must not find it uncancellable.
        lock(&state.jobs.cancels).remove(&id);
    });
    id
}


/// The blueprint a transcript lands in. Its `cues` var is the transcript; the rest is looks.
const SUBTITLES_BLUEPRINT: &str = "subtitles";

/// The folder a new transcript shares: one a transcript in the bin uses (its manifest takes
/// `cues`), else `overlays/subtitles` if an undone one left it. None means copy the blueprint.
fn subtitles_folder(state: &AppState, p: &Project) -> Option<String> {
    let takes_cues = |rel: &str| crate::overlays::resolve(state, rel).is_ok_and(|r| r.manifest.vars.values().any(|v| v.ty == kuma_core::VarType::Cues));
    let left_behind = format!("overlays/{SUBTITLES_BLUEPRINT}");
    let found = p.overlay_folders().into_iter().chain([left_behind.as_str()]).find(|rel| takes_cues(rel)).map(str::to_string);
    found // bound first: the iterator borrows `left_behind`
}

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
    /// Cut the transcript at pauses into entries of about this many seconds, each its own
    /// `subtitles` entry with its own cues and `offset` (`kuma_overlay::split_at_pauses`):
    /// one long entry bakes as one long clip, and again after every correction. Default
    /// [`SPLIT_SECONDS`]; 0 keeps one entry. Several pieces are `<id>-1`, `<id>-2`…, and
    /// from a clip, the pieces it plays, each placed over its stretch. `replace` overwrites
    /// one entry's words, so with it the default is 0 and a split is refused.
    #[serde(default)]
    pub split: Option<f64>,
    #[serde(flatten)]
    pub options: TranscribeOptions,
    /// The edit's origin: the request's `origin`, which [`Asked`] reads, so the handler
    /// sets it.
    #[serde(skip, default = "default_origin")]
    pub origin: String,
}
pub fn default_origin() -> String {
    "ui".into()
}

/// How long a transcript entry is by default, in seconds: past about a minute, a bake is
/// long enough to wait on.
pub const SPLIT_SECONDS: f64 = 60.0;

/// Transcribe a media entry's audio and add the result to the bin as `subtitles` overlay
/// entries under that media, cues in source seconds: one per stretch of about a minute,
/// cut at pauses (`split`). From a clip, also place them over the clip. With `replace`, an
/// entry that is already there keeps everything but its words. One patch either way, so it
/// is revision-checked and one undo step.
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
    let existing = subtitles_folder(state, &project);
    if existing.is_none() {
        state.blueprint_dir(SUBTITLES_BLUEPRINT)?;
    }
    // A courtesy, not the check that counts: refuse an id that is plainly taken before
    // spending minutes on it. The one that decides runs below, against the live project.
    if let Some(id) = req.id.as_ref().filter(|_| !req.replace) {
        anyhow::ensure!(!project.bin.contains_key(id), "`{id}` already exists; pass `replace` to overwrite its transcript, or a different `id`");
    }
    let reversed = clip.as_ref().and_then(|(_, id)| project.find_clip(id)).is_some_and(|(_, _, c)| c.speed < 0.0);
    let split = match req.split {
        Some(s) if !(s == 0.0 || (s.is_finite() && s >= 5.0)) => anyhow::bail!("`split` is the seconds of transcript per entry, 5 or more, or 0 for one entry"),
        Some(s) if s > 0.0 && req.replace => anyhow::bail!("`replace` overwrites the words of one entry and `split` makes several: pass `split: 0` with `replace`, or remove the old transcript and transcribe again to split it"),
        Some(s) if s > 0.0 && reversed => anyhow::bail!("a reversed clip's transcript cannot be split: pass `split: 0`"),
        Some(s) => s,
        None if req.replace || reversed => 0.0,
        None => SPLIT_SECONDS,
    };

    progress(2, "extracting audio");
    let media = resolve_path(state, Path::new(path));
    // The model's own percentage, over most of the job: it is the part that takes minutes.
    let report = |p: u8| progress(5 + (p as u16 * 80 / 100) as u8, &format!("transcribing {p}%"));
    let cues = state.transcriber.transcribe(&media, &state.config.cache_dir().join("transcripts"), &req.options, &report).map_err(|e| anyhow::anyhow!(e))?;
    progress(90, "adding to the bin");

    // Copied outside the edit lock, and only once the words are in.
    let overlay = match existing {
        Some(folder) => folder,
        None => crate::overlays::copy_blueprint(state, SUBTITLES_BLUEPRINT, None)?,
    };
    let pieces = kuma_overlay::split_at_pauses(&cues, split);
    // The language asked for is the page's too, where the folder takes one: kanji then come
    // in Japanese forms without anyone setting it. A detected language is not used, since
    // whisper can guess wrong on a short take.
    let lang_var = crate::overlays::resolve(state, &overlay).ok().and_then(|r| r.manifest.lang_var().map(String::from));
    let lang = req.options.language.clone().filter(|l| !l.is_empty() && l != "auto").zip(lang_var);
    let (outcome, rev, warnings) = state.apply_built(&req.origin, None, |p| {
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
        let made = transcript_pieces(&pieces, &cues, placement.as_ref().map(|(_, c)| c))?;
        let several = made.len() > 1;
        let ids = transcript_ids(p, req, &bin, made.len())?;
        // Placed unless on a timeline already: a re-run must not stack a second clip on
        // the one the first run put there.
        let on_a_timeline: HashSet<&str> = p.all_clips().map(|(_, _, c)| c.bin.as_str()).collect();
        let places: Vec<_> = made.iter().zip(&ids).map(|((_, at), (id, _))| at.zip(placement.as_ref()).filter(|_| !on_a_timeline.contains(id.as_str()))).collect();
        let mut clip_ids = p.free_track_or_clip_ids("subs", places.iter().flatten().count()).into_iter();

        let mut ops = Vec::new();
        // The overlay track the clips go on, found once: they are side by side within the
        // media clip's range.
        let mut track: Option<(String, usize)> = None;
        let mut written = Vec::new();
        for (((piece, at), (id, replacing)), place) in made.iter().zip(&ids).zip(places) {
            let words = serde_json::to_value(&cues[piece.cues.clone()])?;
            // A piece's `offset` is where its stretch starts, placed or not; an unsplit
            // transcript has one only when placed, as it always had.
            let offset = at.map(|(_, _, o)| o).or_else(|| several.then_some(piece.start));
            if *replacing {
                // Only the vars the transcript owns: the entry's own styling stays as it is.
                ops.push(bin_op(id, "/source/vars/cues", words)?);
                if let Some(o) = offset {
                    ops.push(bin_op(id, "/source/vars/offset", o.into())?);
                }
            } else {
                let mut vars = serde_json::Map::from_iter([("cues".to_string(), words)]);
                if let Some(o) = offset {
                    vars.insert("offset".into(), o.into());
                }
                if let Some((code, var)) = &lang {
                    vars.insert(var.clone(), code.clone().into());
                }
                let name = format!("Subtitles: {}", media_item.display_name(&bin));
                let entry = kuma_core::BinItem {
                    name: if several { format!("{name} ({}/{})", written.len() + 1, made.len()) } else { name },
                    source: BinSource::Overlay { overlay: overlay.clone(), vars },
                    meta: None,
                    parent: Some(bin.clone()),
                    order: None,
                    volume: 1.0,
                };
                // The entry first: the clip that places it must find it.
                ops.push(bin_op(id, "", serde_json::to_value(entry)?)?);
            }
            let placed = place.is_some();
            if let Some(((start, end, _), (seq, c))) = place {
                // The overlay clip has the media clip's speed, and starts where its stretch
                // does, so at every timeline instant the overlay's own time is `source −
                // offset`: the cues line up.
                if track.is_none() {
                    let tracks = &p.seq(seq).ok_or_else(|| anyhow::anyhow!("`{seq}` is not a sequence"))?.tracks;
                    let free = |t: &kuma_core::Track| t.kind == kuma_core::TrackKind::Overlay && !t.clips.iter().any(|x| x.start < c.end && x.end > c.start);
                    let ti = match tracks.iter().position(free) {
                        Some(ti) => ti,
                        None => {
                            let new = kuma_core::Track { id: p.free_track_or_clip_id("o"), kind: kuma_core::TrackKind::Overlay, name: "Subtitles".into(), muted: false, hidden: false, clips: vec![], transitions: vec![] };
                            ops.push(add_op(&format!("/bin/{seq}/source/tracks/-"), serde_json::to_value(new)?)?);
                            tracks.len()
                        }
                    };
                    track = Some((seq.clone(), ti));
                }
                let (seq, ti) = track.clone().unwrap_or_default();
                let clip_id = clip_ids.next().unwrap_or_default();
                let placed = kuma_core::Clip { id: clip_id, start, end, bin: id.clone(), source_in: 0.0, speed: c.speed, volume: 1.0, muted: false, effects: vec![], label: String::new(), group: String::new() };
                ops.push(add_op(&format!("/bin/{seq}/source/tracks/{ti}/clips/-"), serde_json::to_value(placed)?)?);
            }
            written.push(Written { id: id.clone(), replaced: *replacing, placed, piece: (*piece).clone() });
        }
        Ok((ops, written))
    })?;
    let first = &outcome[0];
    let placed = outcome.iter().any(|w| w.placed);
    let mut out = serde_json::json!({ "bin": first.id, "cues": cues.len(), "replaced": first.replaced, "placed": placed, "revision": rev, "warnings": warnings });
    if outcome.len() > 1 {
        // Seconds of the source each stands for; the last runs to the end of the words.
        let last = cues.last().map_or(0.0, |c| c.end);
        out["pieces"] = outcome.iter().map(|w| serde_json::json!({ "bin": w.id, "start": secs(w.piece.start.min(last)), "end": secs(w.piece.end.min(last)), "cues": w.piece.cues.len(), "placed": w.placed })).collect();
    }
    Ok(out)
}

/// A transcript entry a run wrote.
struct Written {
    id: String,
    replaced: bool,
    placed: bool,
    piece: kuma_overlay::Piece,
}

/// The pieces a run makes, each with where it goes on the clip's sequence, `(start, end,
/// offset)`. One piece is the clip's whole length; of several, a clip makes those it plays
/// a word of, each over its stretch; a bin entry makes them all, unplaced.
fn transcript_pieces<'a>(pieces: &'a [kuma_overlay::Piece], cues: &[kuma_overlay::Cue], clip: Option<&kuma_core::Clip>) -> anyhow::Result<Vec<(&'a kuma_overlay::Piece, Option<(f64, f64, f64)>)>> {
    Ok(match clip {
        None => pieces.iter().map(|piece| (piece, None)).collect(),
        Some(c) if pieces.len() == 1 => vec![(&pieces[0], Some((c.start, c.end, c.source_in)))],
        Some(c) => {
            let (from, to) = (c.source_in, c.source_in + c.duration() * c.speed);
            let at = |s: f64| c.start + (s - from) / c.speed;
            let made: Vec<_> = pieces
                .iter()
                .filter(|piece| cues[piece.cues.clone()].iter().any(|q| q.end > from && q.start < to))
                .map(|piece| {
                    let (a, b) = (piece.start.max(from), piece.end.min(to));
                    (piece, Some((if a <= from { c.start } else { at(a) }, if b >= to { c.end } else { at(b) }, a)))
                })
                .collect();
            anyhow::ensure!(!made.is_empty(), "clip `{}` plays none of the words", c.id);
            made
        }
    })
}

/// The entries `n` pieces go in, each with whether it is there to be replaced: the entry
/// named, else the one `replace` means, else a free name minted with the words in hand.
/// Several pieces are `<id>-1`, `<id>-2`…, all of them free.
fn transcript_ids(p: &Project, req: &TranscribeRequest, bin: &str, n: usize) -> anyhow::Result<Vec<(String, bool)>> {
    if n > 1 {
        let ids = p.free_bin_ids(req.id.as_deref().unwrap_or(&format!("{bin}-subs")), n);
        if let Some(id) = &req.id {
            anyhow::ensure!(ids[0] == format!("{id}-1"), "`{id}-1` to `{id}-{n}` must all be free to split the transcript under `{id}`; pass a different `id`");
        }
        return Ok(ids.into_iter().map(|id| (id, false)).collect());
    }
    let target = req.id.clone().or_else(|| req.replace.then(|| format!("{bin}-subs")));
    Ok(vec![match target {
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
    }])
}

fn add_op(pointer: &str, value: serde_json::Value) -> anyhow::Result<json_patch::PatchOperation> {
    Ok(json_patch::PatchOperation::Add(json_patch::AddOperation { path: pointer.parse()?, value }))
}

/// `/bin/<id><rest>`, the id escaped so a bad one is refused rather than misplaced.
fn bin_pointer(id: &str, rest: &str) -> String {
    format!("/bin/{}{rest}", id.replace('~', "~0").replace('/', "~1"))
}

/// `add /bin/<id><rest>`.
pub(crate) fn bin_op(id: &str, rest: &str, value: serde_json::Value) -> anyhow::Result<json_patch::PatchOperation> {
    add_op(&bin_pointer(id, rest), value)
}

/// `replace /bin/<id><rest>`.
pub(crate) fn bin_replace_op(id: &str, rest: &str, value: serde_json::Value) -> anyhow::Result<json_patch::PatchOperation> {
    Ok(json_patch::PatchOperation::Replace(json_patch::ReplaceOperation { path: bin_pointer(id, rest).parse()?, value }))
}

pub type Bakes = HashMap<String, OverlayBake>;

/// Which overlays a compile carries bakes for. `Nested` and the live DOM the editor draws
/// partition the overlays between them, so none is ever drawn twice.
///
/// On the wire this is the bool it always was — `Nested` is for the daemon's own callers
/// (the preview, and the snapshot that stands in for it), not for clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(from = "bool", into = "bool")]
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

impl From<OverlayScope> for bool {
    fn from(scope: OverlayScope) -> bool {
        scope != OverlayScope::None
    }
}

/// Overlay clips in the sequences `root` reaches; `nested_only` drops `root`'s own, and
/// `within` keeps only those drawn at one of its times, in frames as the sample renders
/// them. The times are followed through every placement: a nested sequence is asked for
/// the frames of its own that the sampled ones show, so a frame of a master that places a
/// hundred scenes bakes the overlays of the scene it shows and not the other ninety-nine.
///
/// Reachability matters: a bin full of unplaced sequences must cost neither a bake on
/// every preview reload nor a browser run on every render. So does what a track gives: a
/// hidden overlay track is left out of the graph, by the same rule the compiler applies
/// (`Project::track_streams`), so its clips need no bake.
fn overlay_clips(project: &Project, root: &str, nested_only: bool, within: Option<&[f64]>) -> Vec<String> {
    let order = project.compile_order(root).unwrap_or_default();
    let fps = project.fps;
    // The frames of each sequence's own time that are shown; `None` is all of them.
    let mut shown: HashMap<&str, Option<Vec<i64>>> = HashMap::from([(root, within.map(|times| times.iter().map(|&t| fps.frame(t)).collect()))]);
    // Outermost first, so a sequence's frames are settled before it hands them on: every
    // sequence placing another comes after it in `compile_order`.
    for id in order.iter().rev() {
        let (Some(s), Some(mine)) = (project.seq(id), shown.get(id.as_str()).cloned()) else { continue };
        for track in &s.tracks {
            for c in track.clips.iter().filter(|c| project.item(c).is_some_and(kuma_core::BinItem::is_sequence)) {
                let inner = mine.as_ref().map(|frames| {
                    // A dissolve shows a placement past its own edges, by no more than the
                    // transition's length; a freeze shows one frame, whichever is asked for.
                    let pad = track.transitions.iter().filter(|x| x.from == c.id || x.to == c.id).map(|x| fps.length(x.duration)).max().unwrap_or(0);
                    let held = c.effects.iter().find_map(|e| match e {
                        kuma_core::Effect::Freeze { t } => Some(project.held_frame(c, *t, (pad, pad)).0),
                        _ => None,
                    });
                    let f = fps.clip_frames(c.start, c.end);
                    frames.iter().filter(|t| (f.in_ - pad..=f.out + pad).contains(t)).map(|t| held.unwrap_or(c.source_span(fps).0 + t - f.in_)).collect::<Vec<_>>()
                });
                // A sequence placed twice shows what either placement shows.
                let all = shown.entry(c.bin.as_str()).or_insert_with(|| Some(Vec::new()));
                *all = all.take().zip(inner).map(|(mut a, b)| {
                    a.extend(b);
                    a
                });
            }
        }
    }
    order
        .iter()
        .filter(|id| !(nested_only && id.as_str() == root))
        .filter_map(|id| Some((shown.get(id.as_str())?, project.seq(id)?)))
        .flat_map(|(mine, s)| {
            let compiled = |t: &&kuma_core::Track| {
                let (picture, sound) = project.track_streams(t, kuma_core::Streams::Both);
                picture || sound
            };
            s.tracks.iter().filter(compiled).flat_map(move |t| t.clips.iter().map(move |c| (mine, c)))
        })
        .filter(|(mine, c)| {
            mine.as_ref().is_none_or(|frames| {
                let f = fps.clip_frames(c.start, c.end);
                frames.iter().any(|t| (f.in_..=f.out).contains(t))
            })
        })
        .filter(|(_, c)| project.item(c).is_some_and(kuma_core::BinItem::is_overlay))
        .map(|(_, c)| c.id.clone())
        .collect()
}

/// Bake the overlays `scope` asks for. `Nested` is best effort: an overlay that will not
/// bake — or no hyperframes at all — costs you that overlay in the preview and never the
/// preview itself, which used to be free because overlays were skipped entirely.
pub fn bake_for(
    state: &AppState,
    project: &Project,
    root: &str,
    scope: OverlayScope,
    within: Option<&[f64]>,
    progress: &dyn Fn(u8, &str),
    cancel: Option<&AtomicBool>,
) -> anyhow::Result<(Bakes, Vec<Issue>)> {
    let nested_only = match scope {
        OverlayScope::None => return Ok(Default::default()),
        OverlayScope::Nested => true,
        OverlayScope::All => false,
    };
    let (out, errs, warnings) = bake_clips(state, project, &overlay_clips(project, root, nested_only, within), progress, cancel, &|r, key| state.baker.bake_keyed(r, key, cancel).map(Some));
    if !nested_only {
        return strict((out, errs, warnings));
    }
    errs.iter().for_each(|e| tracing::warn!("preview: {e:#}"));
    Ok((out, warnings))
}

/// Bake these overlay clips, best effort: what baked, and why the rest did not. The
/// preview's background bake, which must not fail for one broken overlay.
pub fn bake_ids(state: &AppState, project: &Project, ids: &[String], progress: &dyn Fn(u8, &str)) -> (Bakes, Vec<anyhow::Error>) {
    let _cache = read(&state.cache_users);
    let (out, errs, _) = bake_clips(state, project, ids, progress, None, &|r, key| state.baker.bake_keyed(r, key, None).map(Some));
    (out, errs)
}

/// Bake the named overlay clips, or every one in the project when none are named. This is
/// the explicit `bake` command, where "all" means the whole bin rather than what some
/// timeline reaches — warming the cache for an entry you have not placed yet is the point.
pub fn bake_all(state: &AppState, project: &Project, only: Option<&[String]>, progress: &dyn Fn(u8, &str), cancel: Option<&AtomicBool>) -> anyhow::Result<(Bakes, Vec<Issue>)> {
    let _cache = read(&state.cache_users);
    let ids: Vec<String> = every_overlay_clip(project)
        .map(|c| c.id.clone())
        .filter(|id| only.is_none_or(|o| o.contains(id)))
        .collect();
    strict(bake_clips(state, project, &ids, progress, cancel, &|r, key| state.baker.bake_keyed(r, key, cancel).map(Some)))
}

/// The bakes the cache already holds for every overlay `root` shows, nested ones included,
/// rendering none; and the clips left out because theirs is not there (or cannot be
/// keyed). What `kuma compile` includes without waiting on hyperframes.
pub fn cached_bakes(state: &AppState, project: &Project, root: &str) -> (Bakes, Vec<String>) {
    cached(state, project, overlay_clips(project, root, false, None))
}

/// [`cached_bakes`] for the overlays of the sequences `root` nests only — what the preview
/// bakes, its own being drawn live.
pub fn cached_nested(state: &AppState, project: &Project, root: &str) -> (Bakes, Vec<String>) {
    cached(state, project, overlay_clips(project, root, true, None))
}

fn cached(state: &AppState, project: &Project, ids: Vec<String>) -> (Bakes, Vec<String>) {
    let (out, ..) = bake_clips(state, project, &ids, &|_, _| {}, None, &|r, key| state.baker.cached_keyed(r, key));
    let missing = ids.into_iter().filter(|id| !out.contains_key(id)).collect();
    (out, missing)
}

/// Every overlay clip in every sequence: what `kuma bake` bakes and a prune keeps.
pub(crate) fn every_overlay_clip(project: &Project) -> impl Iterator<Item = &kuma_core::Clip> {
    project.all_clips().map(|(_, _, c)| c).filter(|c| project.item(c).is_some_and(kuma_core::BinItem::is_overlay))
}

/// A bake that must not lose an overlay: the first failure becomes the result.
fn strict((out, errs, warnings): (Bakes, Vec<anyhow::Error>, Vec<Issue>)) -> anyhow::Result<(Bakes, Vec<Issue>)> {
    errs.into_iter().next().map_or(Ok((out, warnings)), Err)
}

/// How `bake_clips` gets a clip's bake: `Baker::bake_keyed`, or `Baker::cached_keyed` to
/// render none, given the key when it could be worked out. Called from several threads.
type Fetch<'a> = &'a (dyn Fn(&BakeRequest, Option<&str>) -> Result<Option<kuma_overlay::BakeResult>, kuma_overlay::BakeError> + Sync);

/// Bake the named overlay clips (cached), returning what succeeded, what did not, and a
/// warning for each bake a script threw in or that drew nothing: the renderer exits 0 on
/// both, and an entry with nothing to show is legal, so neither is a failure.
/// Whether a failure is fatal is the caller's to decide — the loop does not care. A set
/// `cancel` stops it starting another bake, which the cache makes free to pick up again.
/// `fetch` bakes a clip or only looks it up; `None` from it leaves the clip out, no error.
///
/// Clips bake on up to `Baker::concurrency` threads, longest first. Clips that come to one
/// key are fetched once, rather than parking a thread on `Baker::claim` for the length of a
/// bake. Progress comes from this thread alone, and errors in `ids` order.
fn bake_clips(state: &AppState, project: &Project, ids: &[String], progress: &dyn Fn(u8, &str), cancel: Option<&AtomicBool>, fetch: Fetch) -> (Bakes, Vec<anyhow::Error>, Vec<Issue>) {
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::SeqCst));
    // Clips of one entry, and entries of one folder, share a manifest and a digest: read each once.
    let mut folders: HashMap<&str, Result<(crate::overlays::Resolved, Option<String>), String>> = HashMap::new();
    let mut clips = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let Some((_, _, clip)) = project.find_clip(id) else { continue };
        let Some(BinSource::Overlay { overlay, vars }) = project.item(clip).map(|i| &i.source) else { continue };
        folders.entry(overlay.as_str()).or_insert_with(|| {
            let r = crate::overlays::resolve(state, overlay).map_err(|e| format!("{e:#}"))?;
            let digest = state.baker.folder_digest(&r.dir).ok();
            Ok((r, digest))
        });
        clips.push((i, id.as_str(), overlay.as_str(), vars, clip));
    }

    /// One fetch, and the clips (index into `ids`, id) that share its key.
    struct Task<'a> {
        req: BakeRequest<'a>,
        key: Option<String>,
        label: String,
        clips: Vec<(usize, &'a str)>,
    }
    let mut errs = std::collections::BTreeMap::new();
    let mut tasks: Vec<Task> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (i, id, overlay, vars, clip) in clips {
        let (r, digest) = match &folders[overlay] {
            Ok(f) => f,
            Err(e) => {
                errs.insert(i, anyhow::anyhow!("clip `{id}`: {e}"));
                continue;
            }
        };
        let req = bake_request(r, vars, project, clip);
        // A clip that cannot be keyed here gets a fetch of its own, which reports why.
        let key = digest.as_deref().and_then(|d| state.baker.key_with(d, &req).ok());
        if let Some(&t) = key.as_ref().and_then(|k| by_key.get(k)) {
            tasks[t].clips.push((i, id));
            continue;
        }
        if let Some(k) = &key {
            by_key.insert(k.clone(), tasks.len());
        }
        tasks.push(Task { req, key, label: format!("{id} ({overlay})"), clips: vec![(i, id)] });
    }
    // A still is one frame however long its clip. The sort is stable: ties keep `ids` order.
    let frames = |t: &Task| if t.req.manifest.is_static { 0.0 } else { t.req.duration };
    tasks.sort_by(|a, b| frames(b).total_cmp(&frames(a)));

    let (mut out, mut warnings) = (Bakes::new(), Vec::new());
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        // Each worker says when it starts a task (`None`) and when it has its result.
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..state.baker.concurrency.max(1).min(tasks.len()) {
            let (tx, next, tasks) = (tx.clone(), &next, &tasks);
            s.spawn(move || {
                while !cancelled() {
                    let t = next.fetch_add(1, Ordering::SeqCst);
                    let Some(task) = tasks.get(t) else { break };
                    let _ = tx.send((t, None));
                    let _ = tx.send((t, Some(fetch(&task.req, task.key.as_deref()))));
                }
            });
        }
        drop(tx);
        let (mut running, mut done) = (std::collections::BTreeSet::new(), 0);
        for (t, fetched) in rx {
            let task = &tasks[t];
            if fetched.is_none() {
                running.insert(t);
            } else {
                running.remove(&t);
                done += 1;
            }
            match fetched {
                Some(Ok(Some(r))) => {
                    let why = match (r.page_errors.first(), r.blank) {
                        (Some(e), true) => Some(format!("baked fully transparent, a script on the page having thrown: {e}")),
                        (Some(e), false) => Some(format!("a script on the page threw while it baked: {e}")),
                        (None, true) => Some("baked fully transparent: nothing is drawn for its whole length".to_string()),
                        (None, false) => None,
                    };
                    let entry = project.find_clip(task.clips[0].1).map(|(_, _, c)| bin_pointer(&c.bin, ""));
                    if let (Some(why), Some(path)) = (why, entry) {
                        let ids: Vec<&str> = task.clips.iter().map(|&(_, id)| id).collect();
                        warnings.push(Issue { severity: Severity::Warning, path, message: format!("overlay clip `{}` {why}", ids.join("`, `")) });
                    }
                    out.extend(task.clips.iter().map(|(_, id)| (id.to_string(), r.overlay_bake())));
                }
                Some(Err(e)) => errs.extend(task.clips.iter().map(|&(i, id)| (i, anyhow::anyhow!("bake `{id}`: {e}")))),
                _ => {}
            }
            if !running.is_empty() {
                let labels: Vec<&str> = running.iter().map(|&t| tasks[t].label.as_str()).collect();
                progress((done * 100 / tasks.len()) as u8, &format!("baking {}", labels.join(", ")));
            }
        }
    });
    let mut errs: Vec<anyhow::Error> = errs.into_values().collect();
    // Workers only stop short of the end when cancelled.
    if next.into_inner() < tasks.len() {
        errs.push(anyhow::anyhow!("cancelled"));
    }
    // Bakes finish in any order; a report must not.
    warnings.sort_by(|a, b| (&a.path, &a.message).cmp(&(&b.path, &b.message)));
    (out, errs, warnings)
}

/// What an overlay clip bakes as — the one definition, since the prune keys it too.
pub(crate) fn bake_request<'a>(r: &'a crate::overlays::Resolved, vars: &'a serde_json::Map<String, serde_json::Value>, project: &Project, clip: &kuma_core::Clip) -> BakeRequest<'a> {
    BakeRequest { overlay_dir: &r.dir, manifest: &r.manifest, vars, fps: project.fps, width: project.width, height: project.height, duration: clip.duration(), frames: project.fps.clip_frames(clip.start, clip.end).len(), speed: clip.speed }
}

/// Compile options pointed at this project's directories. Callers set what they need on
/// it (`bakes`, `unbaked`, `size`, `sequence`) — a new compile knob is then a field rather
/// than another parameter on every function between here and the caller.
pub fn compile_options(state: &AppState) -> CompileOptions {
    let mut opts = CompileOptions::new(state.config.project_root());
    opts.cache_dir = state.config.compile_cache_dir();
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
    let path = state.config.cache_dir().join(format!("{name}.mlt"));
    write_atomic(&path, xml.as_bytes())?;
    Ok(path)
}

/// Write `bytes` to `path`, whole or not at all: into a `.tmp-…` scratch file beside it
/// (`is_in_flight`), then renamed, so a reader never sees half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = tempfile::Builder::new().prefix(".tmp-").tempfile_in(dir)?;
    std::fs::write(tmp.path(), bytes)?;
    tmp.persist(path).map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;
    Ok(())
}

/// What a render or an export of `sequence` is named after when no output is asked for:
/// `renders/<project>`, then `-<sequence>` unless it is the master. Each adds its suffix.
pub fn output_stem(project: &Project, sequence: &str) -> String {
    let name = if project.name.is_empty() { "kuma" } else { project.name.as_str() };
    let part = if sequence == project.master { String::new() } else { format!("-{sequence}") };
    format!("renders/{name}{part}")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RenderRequest {
    #[serde(default = "default_preset")]
    pub preset: String,
    pub output: Option<PathBuf>,
    /// Seconds; whole timeline when absent.
    pub range: Option<(f64, f64)>,
    /// Sequence to render. The master when absent — rendering a part is the exception.
    #[serde(default)]
    pub sequence: Option<String>,
    /// What this render changes of its preset (`kuma_mlt::Tuning`); each absent is the
    /// preset's own, or the machine's for threads.
    #[serde(default)]
    pub threads: Option<usize>,
    #[serde(default)]
    pub quality: Option<u32>,
    #[serde(default)]
    pub speed: Option<String>,
}
fn default_preset() -> String {
    "youtube-1080p".into()
}

impl Default for RenderRequest {
    fn default() -> Self {
        RenderRequest { preset: default_preset(), output: None, range: None, sequence: None, threads: None, quality: None, speed: None }
    }
}

impl RenderRequest {
    pub fn tuning(&self) -> kuma_mlt::Tuning {
        kuma_mlt::Tuning { threads: self.threads, quality: self.quality, speed: self.speed.clone() }
    }
}

/// A render request with its defaults filled in: sequence, preset, absolute output path.
/// Its tuning, and whether this machine has the preset's encoder, are checked here too, so
/// a bad one is refused when the render is asked for rather than when its turn comes. Can
/// block the first time a hardware preset is asked for, while it is tried.
pub fn render_target(state: &AppState, req: &RenderRequest) -> anyhow::Result<(String, &'static kuma_mlt::ExportPreset, PathBuf)> {
    let project = read(&state.project);
    let preset = kuma_mlt::presets::find(&req.preset).ok_or_else(|| anyhow::anyhow!("unknown preset `{}`", req.preset))?;
    kuma_mlt::presets::available(preset).map_err(|why| anyhow::anyhow!("preset `{}` cannot run on this machine: {why}", preset.id))?;
    preset.check(&req.tuning()).map_err(|e| anyhow::anyhow!(e))?;
    let sequence = project.sequence_or_master(req.sequence.as_deref()).map_err(|e| anyhow::anyhow!(e))?.clone();
    let output = req.output.clone().unwrap_or_else(|| PathBuf::from(format!("{}-{}.{}", output_stem(&project, &sequence), preset.id, preset.extension)));
    let output = if output.is_absolute() { output } else { state.config.project_root().join(output) };
    Ok((sequence, preset, output))
}

/// Run `f` once every render asked for earlier has finished, reporting the start (which
/// makes the job `running`). Cancelled while waiting, it gives up its place.
pub fn in_render_lane<T>(state: &AppState, cancel: &AtomicBool, progress: &dyn Fn(u8, &str), f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    let lane = &state.jobs.lane;
    let mut s = lock(&lane.state);
    s.next += 1;
    let ticket = s.next;
    s.waiting.push_back(ticket);
    loop {
        if cancel.load(Ordering::SeqCst) {
            s.waiting.retain(|t| *t != ticket);
            lane.turn.notify_all();
            anyhow::bail!("cancelled");
        }
        if !s.busy && s.waiting.front() == Some(&ticket) {
            s.waiting.pop_front();
            s.busy = true;
            break;
        }
        // A freed lane and `cancel` both notify; the timeout is a backstop.
        s = lane.turn.wait_timeout(s, std::time::Duration::from_secs(1)).unwrap_or_else(std::sync::PoisonError::into_inner).0;
    }
    drop(s);
    struct Release<'a>(&'a Lane);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            lock(&self.0.state).busy = false;
            self.0.turn.notify_all();
        }
    }
    let _release = Release(lane);
    progress(0, "starting");
    f()
}

/// Render, once the lane is ours.
pub fn render(state: &AppState, req: &RenderRequest, progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<serde_json::Value> {
    in_render_lane(state, cancel, progress, || render_now(state, req, progress, cancel))
}

fn render_now(state: &AppState, req: &RenderRequest, progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<serde_json::Value> {
    let _cache = read(&state.cache_users);
    let (sequence, preset, output) = render_target(state, req)?;
    let project = read(&state.project).clone();
    let mut opts = compile_options(state);
    let baked;
    (opts.bakes, baked) = bake_for(state, &project, &sequence, OverlayScope::All, None, &|p, m| progress(p / 2, m), Some(cancel))?;
    opts.unbaked = UnbakedPolicy::Error;
    opts.sequence = Some(sequence.clone());
    let mlt = compile_project(state, &project, &opts, "render")?;
    let range = req.range.map(|(a, b)| {
        let f = project.fps.clip_frames(a, b);
        (f.in_, f.out)
    });
    let extra = vec![("ar".to_string(), project.sample_rate.to_string())];
    let job = RenderJob { mlt_file: mlt.clone(), output: output.clone(), preset, range, tuning: req.tuning(), extra };
    progress(50, "rendering with melt");
    if let Err(e) = kuma_mlt::render(&job, cancel, |p| progress(50 + p.percent / 2, &format!("frame {}", p.frame))) {
        // A stopped encode leaves nothing behind.
        if matches!(e, kuma_mlt::MeltError::Cancelled) {
            let _ = std::fs::remove_file(&output);
        }
        return Err(e.into());
    }
    // What the render drew in fallback fonts: it rendered, and it is not what was picked.
    let mut warnings = crate::fonts::check(state, &project, &overlay_entries(&project, &sequence));
    warnings.extend(baked);
    Ok(serde_json::json!({ "output": output, "mlt": mlt, "preset": preset.id, "sequence": sequence, "warnings": warnings }))
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MeasureRequest {
    /// Sequence to measure. The master when absent.
    #[serde(default)]
    pub sequence: Option<String>,
    /// `[start, end]` seconds of it; all of it when absent.
    #[serde(default)]
    pub range: Option<(f64, f64)>,
    /// Parts to measure one by one. When absent, every clip of the sequence that places
    /// another sequence: its chapters, if it is built of them.
    #[serde(default)]
    pub sections: Option<Vec<Section>>,
    /// Track ids to silence for this measurement only. See [`audition`].
    #[serde(default)]
    pub mute: Vec<String>,
    /// Track ids to hear alone, wherever they are nested. See [`audition`].
    #[serde(default)]
    pub solo: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Section {
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The clip it is, when it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
}

/// How loud a sequence is, measured rather than modelled: its sound rendered through the
/// real graph — every fade, volume and mix as a render builds them, and no bakes, since
/// overlays make no sound — then metered to EBU R128 (`kuma_overlay::loudness`). The whole,
/// each section, the loudest moments and the silent stretches, in sequence seconds.
///
/// The sequence is compiled as the root, so its own effects — the master stage, for the
/// master — are in what is measured. Loudness and peak are whatever the gains and that
/// stage make them; this is how an agent that cannot listen finds out, before and after
/// rendering.
pub fn measure_audio(state: &AppState, req: &MeasureRequest, progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<serde_json::Value> {
    let mut project = read(&state.project).clone();
    let sequence = project.sequence_or_master(req.sequence.as_deref()).map_err(|e| anyhow::anyhow!(e))?.clone();
    // On the copy that is compiled: listening to one part never costs the document an edit.
    let silenced = audition(&mut project, &sequence, &req.mute, &req.solo)?;
    let total = project.seq_duration(&sequence);
    let (from, to) = req.range.unwrap_or((0.0, total));
    anyhow::ensure!(from >= 0.0 && to > from, "the range must be `[start, end]` with 0 ≤ start < end");
    let to = to.min(total);
    anyhow::ensure!(to > from, "`{sequence}` is {total:.2} s long, so there is nothing after {from} s to measure");
    let fps = project.fps;
    let frames = fps.clip_frames(from, to);
    // Where the file starts, in sequence seconds: the frame it was cut at.
    let offset = frames.in_ as f64 / fps.as_f64();

    let mut opts = compile_options(state);
    opts.sequence = Some(sequence.clone());
    opts.streams = kuma_mlt::Streams::Audio;
    let dir = state.config.cache_dir();
    std::fs::create_dir_all(&dir)?;
    let mlt = tempfile::Builder::new().prefix(".tmp-measure-").suffix(".mlt").tempfile_in(&dir)?;
    std::fs::write(mlt.path(), compile_xml(&project, &opts)?)?;
    let wav = tempfile::Builder::new().prefix(".tmp-measure-").suffix(".wav").tempfile_in(&dir)?;
    let job = RenderJob {
        mlt_file: mlt.path().to_path_buf(),
        output: wav.path().to_path_buf(),
        preset: &kuma_mlt::presets::AUDIO_ONLY,
        range: Some((frames.in_, frames.out)),
        tuning: kuma_mlt::Tuning::default(),
        extra: vec![("ar".to_string(), project.sample_rate.to_string())],
    };
    progress(0, "rendering the sound");
    kuma_mlt::render(&job, cancel, |p| progress((p.percent as u16 * 4 / 5) as u8, &format!("rendering the sound: frame {}", p.frame)))?;
    progress(80, "measuring");
    let scan = kuma_overlay::loudness::scan(wav.path()).map_err(|e| anyhow::anyhow!(e))?;

    let sections = req.sections.clone().unwrap_or_else(|| chapters(&project, &sequence));
    let measured: Vec<serde_json::Value> = sections
        .iter()
        .filter_map(|s| {
            let (a, b) = (s.start.max(from), s.end.min(to));
            (b > a).then(|| {
                let mut v = serde_json::to_value(Section { start: secs(a), end: secs(b), ..s.clone() }).unwrap_or_default();
                v["integrated"] = serde_json::json!(scan.integrated_over(a - offset, b - offset).and_then(db));
                v["true_peak"] = serde_json::json!(db(scan.peak_over(a - offset, b - offset)));
                v
            })
        })
        .collect();
    let hottest: Vec<serde_json::Value> = scan
        .hottest(5, 2.0)
        .iter()
        .map(|s| serde_json::json!({ "t": secs(s.t - kuma_overlay::loudness::STEP + offset), "true_peak": db(s.true_peak), "short_term": db(s.short_term) }))
        .collect();
    let silences: Vec<serde_json::Value> = scan.silences(1.0).iter().map(|(a, b)| serde_json::json!({ "start": secs(a + offset), "end": secs(b + offset) })).collect();
    let mut out = serde_json::json!({
        "sequence": sequence,
        "start": secs(from),
        "end": secs(to),
        "integrated": scan.integrated.and_then(db),
        "range": db(scan.range),
        "true_peak": db(scan.true_peak),
        "sections": measured,
        "hottest": hottest,
        "silences": silences,
        "note": "LUFS and dBTP of the mix as the graph builds it, before encoding; the encoded file's true peak can come out a little higher. `null` is silence.",
    });
    if !req.mute.is_empty() || !req.solo.is_empty() {
        out["mute"] = serde_json::json!(req.mute);
        out["solo"] = serde_json::json!(req.solo);
        out["silenced"] = serde_json::json!(silenced);
    }
    Ok(out)
}

/// Seconds as results give them: to the hundredth, never `-0`.
fn secs(v: f64) -> f64 {
    (v * 100.0).round() / 100.0 + 0.0
}

/// Decibels as results give them: to the tenth, `None` (null) for silence.
fn db(v: f64) -> Option<f64> {
    v.is_finite().then(|| (v * 10.0).round() / 10.0)
}

/// What [`audition`] silenced on top of what the document already does.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Silenced {
    pub tracks: usize,
    pub clips: usize,
}

/// Mute and solo tracks of `project` (a copy about to be compiled, never the document) to
/// hear one part of `root`: a soloed track plays whole, a track leading to one keeps only
/// the clips that lead there, and every other track on the way is muted. Both only add
/// silence, and an unknown id is refused, since it would otherwise measure the whole mix.
pub fn audition(project: &mut Project, root: &str, mute: &[String], solo: &[String]) -> anyhow::Result<Silenced> {
    if mute.is_empty() && solo.is_empty() {
        return Ok(Silenced::default());
    }
    // Innermost first, so a sequence is settled before anything that places it.
    let order = project.compile_order(root).map_err(|ring| anyhow::anyhow!("`{}` contains itself", ring.join(" → ")))?;
    let reached: std::collections::HashSet<&str> = order.iter().map(String::as_str).collect();
    for id in mute.iter().chain(solo) {
        let Some((seq, track)) = project.sequences().find_map(|(s, seq)| seq.tracks.iter().find(|t| &t.id == id).map(|t| (s, t))) else {
            anyhow::bail!("no track `{id}` in this project");
        };
        anyhow::ensure!(reached.contains(seq.as_str()), "track `{id}` is on `{seq}`, which `{root}` does not play");
        anyhow::ensure!(track.kind != kuma_core::TrackKind::Overlay, "track `{id}` is an overlay track, which makes no sound");
        anyhow::ensure!(!(track.muted && solo.contains(id)), "track `{id}` is muted in the project, and a solo does not unmute it: unmute it with an edit to hear it");
        anyhow::ensure!(!(mute.contains(id) && solo.contains(id)), "track `{id}` is both muted and soloed");
    }
    let soloed = |t: &kuma_core::Track| solo.contains(&t.id);
    // The sequences that hold a solo: a soloed track, or a clip placing one that does.
    let mut holds: std::collections::HashSet<String> = std::collections::HashSet::new();
    for id in &order {
        let Some(seq) = project.seq(id) else { continue };
        if seq.tracks.iter().any(|t| !t.muted && (soloed(t) || t.clips.iter().any(|c| !c.muted && holds.contains(&c.bin)))) {
            holds.insert(id.clone());
        }
    }
    anyhow::ensure!(solo.is_empty() || holds.contains(root), "nothing `{root}` plays leads to the soloed tracks: a track or clip on the way is muted");
    let mut silenced = Silenced::default();
    for id in &order {
        let Some(kuma_core::BinItem { source: BinSource::Sequence(seq), .. }) = project.bin.get_mut(id) else { continue };
        for track in &mut seq.tracks {
            if track.muted || soloed(track) || track.kind == kuma_core::TrackKind::Overlay {
                continue;
            }
            if mute.contains(&track.id) || (holds.contains(id) && !track.clips.iter().any(|c| holds.contains(&c.bin))) {
                track.muted = true;
                silenced.tracks += 1;
                continue;
            }
            if holds.contains(id) {
                for clip in track.clips.iter_mut().filter(|c| !c.muted && !holds.contains(&c.bin)) {
                    clip.muted = true;
                    silenced.clips += 1;
                }
            }
        }
    }
    Ok(silenced)
}

/// What to measure of one source, and how finely.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MediaMeasureRequest {
    /// A media entry of the bin.
    #[serde(default)]
    pub bin: Option<String>,
    /// Or a file in the project: relative to it, or absolute under it.
    #[serde(default)]
    pub path: Option<String>,
    /// `[start, end]` source seconds; all of it when absent.
    #[serde(default)]
    pub range: Option<(f64, f64)>,
    /// The loudness over time: `short_term` (3 s windows, the default), `momentary` (400 ms)
    /// or `none`.
    #[serde(default)]
    pub curve: Option<String>,
    /// Seconds between the curve's values, a multiple of 0.1. By default the step of
    /// 0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30 or 60 s that keeps it to [`CURVE_POINTS`] values.
    #[serde(default)]
    pub step: Option<f64>,
    /// What counts as quiet: every sample under this many dBFS (default -35)…
    #[serde(default)]
    pub silence_db: Option<f64>,
    /// …for at least this many seconds (default 0.15).
    #[serde(default)]
    pub silence_min: Option<f64>,
}

/// About how many values a curve has when no step is asked for: one screen of numbers.
pub const CURVE_POINTS: f64 = 120.0;
/// The most a curve may have: a 20-minute file at 1 s.
pub const MAX_CURVE_POINTS: usize = 1200;
/// The most quiet stretches a result lists; the rest are counted.
pub const MAX_SILENCES: usize = 500;

/// How loud one source is, and where it is quiet: a media entry or a file in the project,
/// read by ffmpeg in one pass (`kuma_overlay::loudness::measure`), in the file's own
/// seconds. What is otherwise `ffmpeg -af ebur128` and `silencedetect` by hand: a take's
/// level, a music bed's level over time to key a volume by, the pauses of a voice, and the
/// onset of a sound effect, which is where a silence ends.
///
/// Measured, never stored: the result depends on the stretch and the threshold asked for,
/// and the bin's probed metadata is already a copy of the file that can go stale.
pub fn measure_media(state: &AppState, req: &MediaMeasureRequest, progress: &dyn Fn(u8, &str), cancel: &AtomicBool) -> anyhow::Result<serde_json::Value> {
    let file = match (&req.bin, &req.path) {
        (Some(id), None) => {
            let project = read(&state.project);
            let item = project.bin.get(id).ok_or_else(|| anyhow::anyhow!("no bin entry `{id}`"))?;
            let BinSource::File { path, kind, .. } = &item.source else { anyhow::bail!("`{id}` is not a media file") };
            anyhow::ensure!(*kind != kuma_core::MediaKind::Image, "`{id}` is a still image, which makes no sound");
            resolve_path(state, Path::new(path))
        }
        (None, Some(path)) => resolve_in_project(state, path)?,
        _ => anyhow::bail!("pass exactly one of `bin` or `path`"),
    };
    let name = req.bin.clone().or_else(|| req.path.clone()).unwrap_or_default();
    // Probed now rather than read from the bin: what is measured is the file as it is.
    let probe = kuma_overlay::probe(&file).map_err(|e| anyhow::anyhow!(e))?;
    anyhow::ensure!(probe.meta.has_audio, "`{name}` has no sound");
    let duration = probe.meta.duration.ok_or_else(|| anyhow::anyhow!("cannot tell how long `{name}` is"))?;
    let (from, to) = req.range.unwrap_or((0.0, duration));
    anyhow::ensure!(from.is_finite() && to.is_finite() && from >= 0.0 && to > from, "the range must be `[start, end]` with 0 ≤ start < end");
    let to = to.min(duration);
    anyhow::ensure!(to > from, "`{name}` is {duration:.2} s long, so there is nothing after {from} s to measure");
    let length = to - from;

    let momentary = match req.curve.as_deref().unwrap_or("short_term") {
        "short_term" => Some(false),
        "momentary" => Some(true),
        "none" => None,
        other => anyhow::bail!("`curve` is `short_term`, `momentary` or `none`, not `{other}`"),
    };
    let step = match req.step {
        Some(s) => {
            anyhow::ensure!(s.is_finite() && s >= 0.1, "`step` is seconds, 0.1 or more");
            (s * 10.0).round() / 10.0
        }
        None => [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0].into_iter().find(|s| length / s <= CURVE_POINTS).unwrap_or_else(|| (length / CURVE_POINTS / 60.0).ceil() * 60.0),
    };
    let points = (length / step).floor() as usize;
    if momentary.is_some() && points > MAX_CURVE_POINTS {
        let least = (length / MAX_CURVE_POINTS as f64 * 10.0).ceil() / 10.0;
        anyhow::bail!("a curve every {step} s over {length:.0} s is {points} values, and the most is {MAX_CURVE_POINTS}: pass a `step` of {least} s or more, a shorter `range`, or `curve: none`");
    }
    let d = kuma_overlay::loudness::Quiet::default();
    let quiet = kuma_overlay::loudness::Quiet { db: req.silence_db.unwrap_or(d.db), min: req.silence_min.unwrap_or(d.min) };
    anyhow::ensure!(quiet.db.is_finite() && (-120.0..0.0).contains(&quiet.db), "`silence_db` is dBFS, under 0 (speech pauses are about -35)");
    anyhow::ensure!(quiet.min.is_finite() && quiet.min > 0.0 && quiet.min <= 60.0, "`silence_min` is seconds, more than 0");

    progress(0, "reading the sound");
    let report = |t: f64| progress((t / length * 100.0).clamp(0.0, 99.0) as u8, &format!("metering {t:.0} of {length:.0} s"));
    let m = kuma_overlay::loudness::measure(&file, req.range.map(|_| (from, to)), quiet, cancel, &report).map_err(|e| anyhow::anyhow!(e))?;

    let curve = momentary.map(|momentary| {
        let values: Vec<Option<f64>> = m.scan.curve(step, momentary).into_iter().map(|v| db(v).filter(|&v| v > kuma_overlay::loudness::SILENT_LUFS)).collect();
        serde_json::json!({
            "of": if momentary { "momentary" } else { "short_term" },
            "window": if momentary { 0.4 } else { 3.0 },
            "start": secs(from + step),
            "step": step,
            "lufs": values,
        })
    });
    let silences: Vec<serde_json::Value> = m.silences.iter().take(MAX_SILENCES).map(|&(a, b)| serde_json::json!({ "start": secs(a), "end": secs(b) })).collect();
    let mut out = serde_json::json!({
        "bin": req.bin,
        "path": project_relative(state, &file),
        "start": secs(from),
        "end": secs(to),
        "integrated": m.scan.integrated.and_then(db),
        "range": db(m.scan.range),
        "true_peak": db(m.scan.true_peak),
        "curve": curve,
        "silence": { "db": quiet.db, "min": quiet.min },
        "silences": silences,
        "note": "Seconds are the file's own. `curve.lufs[i]` is the loudness of the `window` seconds ending at `curve.start + i·step`, null under -70 LUFS. A silence's `start` is where sound stops, its `end` where it starts again (an onset).",
    });
    if m.silences.len() > MAX_SILENCES {
        out["silences_total"] = m.silences.len().into();
    }
    Ok(out)
}

/// A sequence's chapters: its clips that place another sequence, in time order. What
/// `measure_audio` measures one by one, and what `export::chapters` lists for YouTube.
pub(crate) fn chapters(project: &Project, sequence: &str) -> Vec<Section> {
    let Some(seq) = project.seq(sequence) else { return Vec::new() };
    let mut out: Vec<Section> = seq
        .clips()
        .filter_map(|c| {
            let item = project.item(c).filter(|i| i.is_sequence())?;
            let label = if c.label.is_empty() { item.display_name(&c.bin).to_string() } else { c.label.clone() };
            Some(Section { start: c.start, end: c.end, label: Some(label), clip: Some(c.id.clone()) })
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// The overlay entries whose clips `root` shows, nested sequences included, each once.
pub fn overlay_entries<'p>(project: &'p Project, root: &str) -> Vec<&'p str> {
    let mut ids: Vec<&str> = overlay_clips(project, root, false, None).iter().filter_map(|c| project.find_clip(c)).map(|(_, _, c)| c.bin.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// One frame of the master at t=0, overlays included, by default. Callers name what they
/// change; adding a field here no longer means visiting every construction.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SampleRequest {
    pub t0: f64,
    #[serde(default)]
    pub t1: Option<f64>,
    /// Frames evenly spaced over `[t0, t1]`. One without a range; across a range, enough
    /// to see it move (`RANGE_COUNT`) — a range asked for one frame shows none of it.
    #[serde(default)]
    pub count: Option<usize>,
    /// Crop rectangle in composition pixels.
    #[serde(default)]
    pub rect: Option<Region>,
    /// Output width (keeps aspect). See [`SampleRequest::frame_width`].
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
    /// How long to run that overlay for. The overlay's own default when absent — an
    /// entry has no length until a clip gives it one.
    #[serde(default)]
    pub duration: Option<f64>,
    /// What to composite it over, as a colour entry would spell it (`#102030`). Black by
    /// default, and `transparent` for straight alpha: an overlay is transparent, and a
    /// frame of it over nothing is not a picture of anything.
    #[serde(default)]
    pub backdrop: Option<String>,
    /// With `overlay`: the frame to render into, when it is not the project's. An overlay
    /// has to look right at 1920×1080 and 1080×1920 alike, and checking the other way
    /// round should not mean editing the project to find out.
    #[serde(default)]
    pub frame_width: Option<u32>,
    #[serde(default)]
    pub frame_height: Option<u32>,
    /// Frames at exactly these times, in place of `t0`, `t1` and `count`: every cut and a
    /// moment either side of it, the key beat of each scene. Each bakes only the overlays
    /// it shows, not everything between the first and the last.
    #[serde(default)]
    pub times: Option<Vec<f64>>,
    /// Also tile the frames into one image, captioned with their times (`contact_sheet`).
    #[serde(default)]
    pub tile: bool,
    /// The sheet's columns; about square when absent.
    #[serde(default)]
    pub columns: Option<u32>,
}
/// How many frames a range is sampled at when no count is given.
pub const RANGE_COUNT: usize = 5;
/// The most frames one sample renders.
pub const MAX_FRAMES: usize = 32;
/// How wide a frame on a sheet, or each of several, is when no width is asked for.
pub const SHEET_TILE_WIDTH: u32 = 480;
/// How wide one full frame is when no width is asked for.
pub const ONE_FRAME_WIDTH: u32 = 960;

impl SampleRequest {
    /// The width `frames` frames come back at: as asked, else a crop's own, else 960 for
    /// one frame (to look closely) and [`SHEET_TILE_WIDTH`] each for several or a sheet's
    /// (to compare moments, at a quarter of the pixels).
    pub fn frame_width(&self, frames: usize) -> Option<u32> {
        if self.width.is_some() || self.rect.is_some() {
            return self.width;
        }
        Some(if self.tile || frames > 1 { SHEET_TILE_WIDTH } else { ONE_FRAME_WIDTH })
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
pub fn sample_frames(state: &AppState, req: &SampleRequest, progress: &dyn Fn(u8, &str), cancel: Option<&AtomicBool>) -> anyhow::Result<Vec<Sample>> {
    Ok(sample_frames_warned(state, req, progress, cancel)?.0)
}

/// [`sample_frames`], with the warnings of the bakes the frames show.
fn sample_frames_warned(state: &AppState, req: &SampleRequest, progress: &dyn Fn(u8, &str), cancel: Option<&AtomicBool>) -> anyhow::Result<(Vec<Sample>, Vec<Issue>)> {
    let doc = read(&state.project).clone();
    sample_frames_of(state, doc, req, progress, cancel)
}

/// [`sample_frames`] of `doc`, a project that need not be the open one: the open one with
/// something taken out for a measurement, which is never stored.
pub(crate) fn sample_frames_of(state: &AppState, doc: Project, req: &SampleRequest, progress: &dyn Fn(u8, &str), cancel: Option<&AtomicBool>) -> anyhow::Result<(Vec<Sample>, Vec<Issue>)> {
    let _cache = read(&state.cache_users);
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
    let times: Vec<f64> = match &req.times {
        Some(times) => {
            anyhow::ensure!((1..=MAX_FRAMES).contains(&times.len()), "`times` takes 1 to {MAX_FRAMES} times, not {}", times.len());
            anyhow::ensure!(times.iter().all(|t| t.is_finite() && *t >= 0.0), "every time in `times` is seconds, 0 or more");
            times.clone()
        }
        None => {
            let t1 = req.t1.unwrap_or(req.t0);
            let count = req.count.unwrap_or(if t1 > req.t0 { RANGE_COUNT } else { 1 }).clamp(1, MAX_FRAMES);
            (0..count).map(|i| if count == 1 { req.t0 } else { req.t0 + (t1 - req.t0) * i as f64 / (count - 1) as f64 }).collect()
        }
    };
    // Only the overlays this sample can actually show: one frame must not pay for a bake
    // of every overlay on the timeline, nor fail because one somewhere else is broken.
    let (bakes, warnings) = bake_for(state, &project, &sequence, scope, Some(&times), &|p, m| progress(p / 2, m), cancel)?;
    let out_dir = state.config.samples_dir();
    std::fs::create_dir_all(&out_dir)?;
    let mut opts = compile_options(state);
    opts.bakes = bakes;
    opts.sequence = Some(sequence.clone());
    let xml = compile_xml(&project, &opts)?;
    // The frame cache is keyed on the compiled document, which is the whole input: every
    // producer, the bake directories (content hashes, so an overlay edited on disk
    // changes it without a project revision), and whatever the compiler itself does, so a
    // kuma that compiles differently never serves a frame an older one sampled.
    let doc_stamp = hash_str(&xml);
    let mlt_file = tempfile::Builder::new().prefix("sample-").suffix(".mlt").tempfile_in(state.config.cache_dir())?;
    std::fs::write(mlt_file.path(), xml)?;
    let width = req.frame_width(times.len());
    let one = |frame: i64| -> anyhow::Result<PathBuf> {
        let stamp = format!("{doc_stamp}_{frame}");
        let full = out_dir.join(format!("f{stamp}.png"));
        publish(&out_dir, &full, &|tmp| Ok(kuma_mlt::melt::snapshot(mlt_file.path(), frame, tmp)?))?;
        if req.rect.is_none() && width.is_none() {
            return Ok(full);
        }
        let mut vf = Vec::new();
        if let Some(r) = req.rect {
            vf.push(format!("crop={}:{}:{}:{}", r.w, r.h, r.x, r.y));
        }
        if let Some(w) = width {
            vf.push(format!("scale={w}:-2"));
        }
        let p = out_dir.join(format!("f{stamp}_{}.png", hash_str(&vf.join(","))));
        publish(&out_dir, &p, &|tmp| {
            kuma_overlay::media::ffmpeg(&["-i".into(), full.to_string_lossy().into_owned(), "-vf".into(), vf.join(","), tmp.to_string_lossy().into_owned()]).map_err(|e| anyhow::anyhow!(e))
        })?;
        Ok(p)
    };
    // Each frame is a melt run of its own, which loads the whole document: several at once,
    // as a render builds several frames at once, and once however many times round to it.
    // Progress is reported from this thread.
    let mut frames: Vec<i64> = times.iter().map(|&t| project.fps.frame(t)).collect();
    frames.sort_unstable();
    frames.dedup();
    let workers = kuma_mlt::melt::render_threads().clamp(1, 4).min(frames.len());
    let next = AtomicUsize::new(0);
    let mut rendered = HashMap::new();
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..workers {
            let (tx, next, frames, one) = (tx.clone(), &next, &frames, &one);
            s.spawn(move || {
                while let Some(&frame) = frames.get(next.fetch_add(1, Ordering::SeqCst)) {
                    let _ = tx.send((frame, one(frame)));
                }
            });
        }
        drop(tx);
        for (done, (frame, path)) in rx.into_iter().enumerate() {
            progress((50 + 50 * (done + 1) / frames.len()) as u8, &format!("frame {frame}"));
            rendered.insert(frame, path);
        }
    });
    let paths: HashMap<i64, PathBuf> = rendered.into_iter().map(|(f, p)| Ok((f, p?))).collect::<anyhow::Result<_>>()?;
    Ok((times.iter().map(|&t| (t, project.fps.frame(t))).map(|(t, frame)| Sample { t, frame, path: paths[&frame].clone() }).collect(), warnings))
}

/// Produce `target` in `dir` through `f(tmp)` and publish it with a rename, unless it is
/// there already: a reader never sees a half-written file, and scratch is named `.tmp-…`.
fn publish(dir: &Path, target: &Path, f: &dyn Fn(&Path) -> anyhow::Result<()>) -> anyhow::Result<()> {
    if target.is_file() {
        return Ok(());
    }
    let tmp = tempfile::Builder::new().prefix(".tmp-").suffix(".png").tempfile_in(dir)?;
    f(tmp.path())?;
    tmp.persist(target)?;
    Ok(())
}

/// What `/api/sample` answers: the frames, and with `tile` the sheet of them.
pub fn sample(state: &AppState, req: &SampleRequest, progress: &dyn Fn(u8, &str), cancel: Option<&AtomicBool>) -> anyhow::Result<serde_json::Value> {
    let (samples, warnings) = sample_frames_warned(state, req, progress, cancel)?;
    let sheet = if req.tile { Some(contact_sheet(state, &samples, req.columns)?) } else { None };
    Ok(serde_json::json!({ "samples": samples, "sheet": sheet, "warnings": warnings }))
}

/// A sample's frames tiled into one image, left to right then down, each captioned with its
/// time: a contact sheet, so looking at a dozen moments is one image rather than a dozen.
/// Cached beside the frames, under the frames it holds.
pub fn contact_sheet(state: &AppState, samples: &[Sample], columns: Option<u32>) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(!samples.is_empty(), "no frames to tile");
    let n = samples.len();
    let cols = columns.map_or_else(|| (n as f64).sqrt().ceil() as usize, |c| c as usize).clamp(1, n);
    let paths: Vec<String> = samples.iter().map(|s| s.path.to_string_lossy().into_owned()).collect();
    // Named by the frames and the captions: two times can round to one frame.
    let named: Vec<String> = samples.iter().zip(&paths).map(|(s, p)| format!("{p}@{:.2}", s.t)).collect();
    let dir = state.config.samples_dir();
    let out = dir.join(format!("s{}_{cols}.png", hash_str(&named.join("\n"))));
    publish(&dir, &out, &|tmp| {
        let probe = kuma_overlay::probe(&samples[0].path).map_err(|e| anyhow::anyhow!(e))?;
        let (w, h) = (probe.meta.width.unwrap_or_default(), probe.meta.height.unwrap_or_default());
        anyhow::ensure!(w > 0 && h > 0, "cannot read the size of {}", samples[0].path.display());
        // Every frame of a sample is the same size, so a tile's place is its column and row.
        let layout: Vec<String> = (0..n).map(|i| format!("{}_{}", (i % cols) as u32 * w, (i / cols) as u32 * h)).collect();
        let run = |captions: bool| {
            let mut graph = String::new();
            for (i, s) in samples.iter().enumerate() {
                let caption = format!("drawtext=text='{:.2} s':x=8:y=8:fontsize={}:fontcolor=white:box=1:boxcolor=black@0.6:boxborderw=5", s.t, (h / 14).max(10));
                graph.push_str(&format!("[{i}:v]{}[t{i}];", if captions { caption.as_str() } else { "null" }));
            }
            let tiles: String = (0..n).map(|i| format!("[t{i}]")).collect();
            graph.push_str(&if n == 1 { "[t0]null".to_string() } else { format!("{tiles}xstack=inputs={n}:layout={}:fill=0x202020", layout.join("|")) });
            let mut args: Vec<String> = paths.iter().flat_map(|p| ["-i".to_string(), p.clone()]).collect();
            args.extend(["-filter_complex".into(), graph, "-frames:v".into(), "1".into(), "-update".into(), "1".into(), tmp.to_string_lossy().into_owned()]);
            kuma_overlay::media::ffmpeg(&args)
        };
        // Captions need a font ffmpeg can find; a sheet without them is still a sheet.
        run(true)
            .or_else(|e| {
                tracing::warn!("contact sheet without captions: {e}");
                run(false)
            })
            .map_err(|e| anyhow::anyhow!(e))
    })?;
    Ok(out)
}

/// An overlay entry as the workbench or an agent asks to see it: at some length, in some
/// frame. Both are views, not document values — an entry has no length until a clip
/// gives it one, and the project's frame is only the default — so they are resolved
/// against the overlay's manifest and the project here, and **clamped rather than validated**:
/// nothing is stored, and a silly number costs one render.
#[derive(Debug, Clone)]
pub struct EntryView {
    /// The entry's overlay folder, resolved.
    pub overlay: crate::overlays::Resolved,
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
    /// The entry's folder is missing or its manifest does not load.
    #[error("{0}")]
    BadOverlay(String),
}

pub fn entry_view(state: &AppState, doc: &Project, id: &str, duration: Option<f64>, width: Option<u32>, height: Option<u32>) -> Result<EntryView, EntryError> {
    let item = doc.bin.get(id).ok_or_else(|| EntryError::NoEntry(id.into()))?;
    let kuma_core::BinSource::Overlay { overlay, vars } = &item.source else {
        return Err(EntryError::NotOverlay(id.into()));
    };
    let overlay = crate::overlays::resolve(state, overlay).map_err(|e| EntryError::BadOverlay(format!("{e:#}")))?;
    let default = overlay.manifest.duration_default;
    Ok(EntryView {
        overlay,
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
    let entry = |source| kuma_core::BinItem { name: String::new(), source, meta: None, parent: None, order: None, volume: 1.0 };
    let sequence = kuma_core::Sequence {
        tracks: vec![
            track(&format!("{seq_id}-v"), kuma_core::TrackKind::Video, clip(&format!("{seq_id}-bgc"), &bg_id)),
            track(&format!("{seq_id}-o"), kuma_core::TrackKind::Overlay, clip(&format!("{seq_id}-oc"), id)),
        ],
        markers: Vec::new(),
        effects: Vec::new(),
    };
    // The rate and the revision are the real project's — an overlay is sampled as the
    // video it is going into would show it, and the revision keeps the frame cache honest
    // when the entry's vars change — and so is the frame unless the caller asked for
    // another one to check the other orientation in.
    let mut out = doc.clone();
    (out.width, out.height) = frame;
    out.bin = [
        // Out of whatever folder it is filed under, which this bin does not hold: the
        // folder is where the bin lists it, nothing it shows.
        (id.to_string(), kuma_core::BinItem { parent: None, ..item.clone() }),
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

/// A user-supplied path that must be inside the project (relative to it, or absolute and
/// under it), resolved and canonical.
pub fn resolve_in_project(state: &AppState, path: &str) -> anyhow::Result<PathBuf> {
    let file = resolve_path(state, Path::new(path)).canonicalize().map_err(|e| anyhow::anyhow!("cannot read `{path}`: {e}"))?;
    anyhow::ensure!(file.starts_with(state.config.project_root().canonicalize()?), "`{path}` is not inside the project");
    Ok(file)
}

/// `p` as the project stores it: relative when it is inside the project, as given or
/// under its real path.
pub fn project_relative(state: &AppState, p: &Path) -> String {
    let root = state.config.project_root();
    let real = root.canonicalize().unwrap_or_else(|_| root.clone());
    p.strip_prefix(&root).or_else(|_| p.strip_prefix(&real)).unwrap_or(p).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every job says who asked, as an edit does; only the agent's is a call to follow.
    #[test]
    fn a_job_request_says_who_asked() {
        let by = |body: serde_json::Value| serde_json::from_value::<Asked<serde_json::Value>>(body).unwrap().split().0;
        let mcp = by(json!({ "origin": "mcp: render", "call": { "tool": "render", "client": "claude-code" }, "preset": "h264" })).unwrap();
        assert_eq!(serde_json::to_value(&mcp).unwrap(), json!({ "author": "agent", "origin": "mcp: render", "tool": "render", "client": "claude-code" }));
        assert_eq!(mcp.call("render"), Some(Call { tool: "render".into(), client: "claude-code".into() }));
        let ui = by(json!({ "origin": "ui: render", "preset": "h264" })).unwrap();
        assert_eq!((ui.author.as_str(), &ui.tool, ui.call("render")), ("user", &None, None));
        assert_eq!(by(json!({ "origin": "cli: bake" })).unwrap().author, "other");
        // An agent's origin without a call is still followed, under the job's kind.
        assert_eq!(by(json!({ "origin": "agent: tidy" })).unwrap().call("prune"), Some(Call { tool: "prune".into(), client: "agent".into() }));
        // A call alone is an agent's.
        let bare = by(json!({ "call": { "tool": "bake_overlay" } })).unwrap();
        assert_eq!((bare.author.as_str(), bare.origin.as_str(), bare.client.as_deref()), ("agent", "agent: bake_overlay", Some("agent")));
        assert_eq!(by(json!({ "preset": "h264" })), None, "an old client says nothing");
        assert_eq!(by(json!({ "origin": " " })), None);
        let (_, req) = serde_json::from_value::<Asked<TranscribeRequest>>(json!({ "bin": "talk", "origin": "ui: transcribe" })).unwrap().split();
        assert_eq!((req.bin.as_deref(), req.origin.as_str()), (Some("talk"), "ui"), "the handler sets a transcript's origin");
    }

    fn seq(tracks: serde_json::Value) -> serde_json::Value {
        json!({ "source": { "tracks": tracks } })
    }

    fn overlay_track(id: &str, hidden: bool, clips: serde_json::Value) -> serde_json::Value {
        json!({ "id": id, "kind": "overlay", "hidden": hidden, "clips": clips })
    }

    /// A sequence of two overlay clips, two seconds each.
    fn head_and_tail() -> serde_json::Value {
        seq(json!([overlay_track("io1", false, json!([
            { "id": "o_head", "start": 0, "end": 2, "bin": "tag" },
            { "id": "o_tail", "start": 2, "end": 4, "bin": "tag" }
        ]))]))
    }

    fn clip(id: &str, bin: &str) -> serde_json::Value {
        json!({ "id": id, "start": 0, "end": 4, "bin": bin })
    }

    /// The preview bakes the overlays it cannot drive and nothing else. Its own are live
    /// DOM (baking them too would draw every overlay twice), and a sequence sitting
    /// unplaced in the bin must cost neither a bake on every reload nor a browser run on
    /// every render, just for being there; nor must a hidden overlay track, which the
    /// compiler leaves out (`Project::track_streams`, the one rule both follow).
    #[test]
    fn a_preview_bakes_the_overlays_it_nests_and_no_others() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "tag": { "source": { "overlay": "overlays/location-tag" } },
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
        assert_eq!(overlay_clips(&p, "main", true, None), vec!["o_nested"]);
        // Open the inner sequence and the same overlays become live DOM instead.
        assert!(overlay_clips(&p, "inner", true, None).is_empty());
        // A render of the master wants its own overlay as well — but still nothing from
        // the sequence nobody placed, which would be a headless browser run for nothing.
        // Innermost first, the order `compile_order` walks in.
        assert_eq!(overlay_clips(&p, "main", false, None), vec!["o_nested", "o_own"]);
    }

    /// Sampling one frame must not bake every overlay on the timeline, on the sequence
    /// sampled or in the ones it nests: the sampled frames are followed through each
    /// placement into the nested sequence's own time.
    #[test]
    fn a_time_window_narrows_the_root_and_what_it_nests() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "tag": { "source": { "overlay": "overlays/location-tag" } },
                "inner": head_and_tail(),
                "main": seq(json!([
                    overlay_track("mo1", false, json!([
                        { "id": "o_early", "start": 0, "end": 2, "bin": "tag" },
                        { "id": "o_late", "start": 50, "end": 52, "bin": "tag" }
                    ])),
                    json!({ "id": "v1", "clips": [
                        { "id": "s1", "start": 0, "end": 4, "bin": "inner" },
                        { "id": "s2", "start": 20, "end": 22, "bin": "inner", "in": 2 }
                    ] })
                ]))
            }
        }))
        .unwrap();
        assert_eq!(overlay_clips(&p, "main", false, Some(&[0.0, 1.0])), vec!["o_head", "o_early"]);
        assert_eq!(overlay_clips(&p, "main", false, Some(&[3.0])), vec!["o_tail"]);
        // The second placement starts two seconds in: its first frame is the tail's.
        assert_eq!(overlay_clips(&p, "main", false, Some(&[20.0])), vec!["o_tail"]);
        // Each frame bakes what it shows, not everything between the first and the last.
        assert_eq!(overlay_clips(&p, "main", false, Some(&[1.0, 51.0])), vec!["o_head", "o_early", "o_late"]);
        // Where the nested sequence is not on screen, nothing of it is baked.
        assert!(overlay_clips(&p, "main", false, Some(&[10.0, 30.0])).is_empty());
        // In frames: 49.99 s is frame 1500 at 30 fps, the first of a clip starting at 50 s.
        assert_eq!(overlay_clips(&p, "main", false, Some(&[49.99])), vec!["o_late"]);
        // No window, no narrowing: a render wants every one.
        assert_eq!(overlay_clips(&p, "main", false, None), vec!["o_head", "o_tail", "o_early", "o_late"]);
    }

    /// A placement can show other frames of a sequence than its own span: a dissolve reads
    /// past its edge, and a freeze holds one frame whichever is sampled.
    #[test]
    fn a_dissolve_or_a_freeze_keeps_what_the_placement_may_show() {
        let project = |clip: serde_json::Value, transitions: serde_json::Value| -> Project {
            serde_json::from_value(json!({
                "bin": {
                    "tag": { "source": { "overlay": "overlays/location-tag" } },
                    "black": { "source": { "color": "#000000" } },
                    "inner": head_and_tail(),
                    "main": seq(json!([{ "id": "v1", "transitions": transitions, "clips": [ { "id": "b1", "start": 0, "end": 10, "bin": "black" }, clip ] }]))
                }
            }))
            .unwrap()
        };
        let placed = json!({ "id": "s1", "start": 10, "end": 12, "bin": "inner", "in": 2 });
        let plain = project(placed.clone(), json!([]));
        assert!(overlay_clips(&plain, "main", false, Some(&[9.9])).is_empty());
        // Half a second before the cut the dissolve already shows the incoming sequence,
        // from before its `in`.
        let dissolved = project(placed, json!([{ "from": "b1", "to": "s1", "duration": 1.0 }]));
        assert_eq!(overlay_clips(&dissolved, "main", false, Some(&[9.9])), vec!["o_head"]);
        let frozen = project(json!({ "id": "s1", "start": 10, "end": 12, "bin": "inner", "effects": [{ "type": "freeze", "t": 3.0 }] }), json!([]));
        assert_eq!(overlay_clips(&frozen, "main", false, Some(&[10.5])), vec!["o_tail"]);
    }

    /// One frame comes back at 960 px and several at 480 each, the size a sheet's are,
    /// unless a width is asked for; a crop keeps its pixels.
    #[test]
    fn several_frames_come_back_smaller_than_one() {
        let plain = SampleRequest::default();
        assert_eq!(plain.frame_width(1), Some(960));
        assert_eq!(plain.frame_width(5), Some(480));
        assert_eq!(SampleRequest { width: Some(1280), ..Default::default() }.frame_width(2), Some(1280), "asked for wins");
        assert_eq!(SampleRequest { tile: true, ..Default::default() }.frame_width(1), Some(480), "a sheet's frames");
        let rect = Some(Region { x: 0, y: 0, w: 400, h: 300 });
        assert_eq!(SampleRequest { rect, ..Default::default() }.frame_width(1), None, "a crop keeps its pixels");
    }
}
