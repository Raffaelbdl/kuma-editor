//! MCP tools. Each tool is a thin call into the daemon API; the interesting one is
//! `sample_frames`, which returns rendered frames as image content so the agent can look
//! at the actual composite.

use std::time::{Duration, Instant};

use base64::Engine as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ProgressNotificationParam, ProgressToken, ServerCapabilities, ServerInfo};
use rmcp::service::RequestContext;
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, Peer, RoleServer, ServerHandler};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::Client;

/// What every agent sees. Clients truncate server instructions (Claude Code at 2048
/// characters, which once cut these off mid-rule), so this is the invariants and a pointer;
/// the rest is `timeline_guide`, which a client never truncates. The guide restates these
/// rules in full, so change a rule in both.
const INSTRUCTIONS: &str = r#"kuma-editor: a hybrid video editor. **Call `timeline_guide` before your first edit** and
`overlay_guide` before writing an overlay; these instructions are only what must never be missed.

- The project is JSON (`get_project`, schema from `get_schema`). Edit with `apply_edits` (JSON Patch),
  passing the `base_revision` you read and a `note`. Every edit is validated atomically and undoable.
- Times are seconds (f64); a clip's `end` is exclusive; `fps` is rational.
- Everything placeable is a `bin` entry (media, overlays, colours, sequences). A clip is a placement:
  `bin`, `start`, `end`, `in`, `speed`, `effects`, never a source. What is shown lives on the entry.
- There is no project-level `tracks`: a timeline is a sequence entry and `master` names the one that
  renders, so a clip lives at `/bin/<sequence>/source/tracks/<i>/clips/<j>`. Track and clip ids are
  unique across the whole project. A sequence cannot contain itself and cannot be sped up.
- `sample_frames` shows you composited frames; `render` exports. Both default to the master; pass
  `sequence` for another, or `overlay` to `sample_frames` to look at one overlay entry on its own.
- Add media with `add_media`. An overlay reaches the bin through `import_overlay` or `add_blueprint`:
  writing a folder does not add it.
"#;

const TIMELINE_GUIDE: &str = include_str!("timeline_guide.md");

#[derive(Clone)]
pub struct KumaServer {
    client: Client,
    note: Option<String>,
    build: Build,
    tool_router: ToolRouter<Self>,
}

/// What an agent is told on every call once the kuma this server runs is not the one on
/// disk any more. A client lists a server's tools once, so a server that outlives a rebuild
/// keeps the old tools and the old options — which a newer daemon would have taken — and
/// an option the old schema lacks is dropped without a word.
const REBUILT: &str = "kuma was rebuilt after this MCP server started, so these tools are the old build's: an option or tool the rebuild added is missing here, and a daemon this server started is the old one too. Tell the user, and ask them to restart the kuma MCP server (in Claude Code: `/mcp`, then reconnect kuma).";

/// The executable a server runs from, as it was when the server started.
#[derive(Clone, Debug)]
struct Build {
    path: Option<std::path::PathBuf>,
    stamp: Option<(u64, std::time::SystemTime)>,
}

impl Build {
    fn of(path: Option<std::path::PathBuf>) -> Self {
        let stamp = path.as_deref().and_then(stamp);
        Build { path, stamp }
    }

    /// This process's executable. Linux names one that has been replaced `<path> (deleted)`,
    /// and the path it had is where the new build is.
    fn current() -> Self {
        let exe = std::env::current_exe().ok().map(|p| std::path::PathBuf::from(p.to_string_lossy().trim_end_matches(" (deleted)")));
        Build::of(exe)
    }

    /// Whether the file at the path is no longer the one this started from: another size or
    /// time, or gone (a build in progress).
    fn replaced(&self) -> bool {
        self.path.as_deref().is_some_and(|p| stamp(p) != self.stamp)
    }
}

fn stamp(path: &std::path::Path) -> Option<(u64, std::time::SystemTime)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// `response`, with `note` put before whatever it says.
fn noted(mut response: Result<CallToolResponse, McpError>, note: &str) -> Result<CallToolResponse, McpError> {
    match &mut response {
        Ok(CallToolResponse::Complete(r)) => r.content.insert(0, ContentBlock::text(note)),
        Err(e) => e.message = format!("{note}\n\n{}", e.message).into(),
        _ => {}
    }
    response
}

impl KumaServer {
    pub fn new(client: Client, note: Option<String>) -> Self {
        Self { client, note, build: Build::current(), tool_router: Self::tool_router() }
    }

    async fn get(&self, path: &str) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get(path).await.map_err(err)?))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.post(path, body).await.map_err(err)?))
    }

    /// Start a background job and wait for it (up to `timeout`).
    async fn job(&self, path: &str, body: &Value, timeout: Duration, ctx: &RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let v = self.client.post(path, body).await.map_err(err)?;
        let id = v["job"].as_u64().ok_or_else(|| err(anyhow::anyhow!("daemon did not return a job id: {v}")))?;
        let j = self.wait(id, timeout, ctx).await.map_err(err)?;
        if j["state"] == "failed" {
            return Err(err(anyhow::anyhow!("job {id} failed: {}", j["message"].as_str().unwrap_or("?"))));
        }
        Ok(ok_json(&resumable(j)))
    }

    /// Wait up to `timeout` for job `id` to be final and return it as it then is, telling
    /// the client how it is going on the way when the call asked for progress. Stops early
    /// when the client cancels the call: the job itself runs on in the daemon.
    async fn wait(&self, id: u64, timeout: Duration, ctx: &RequestContext<RoleServer>) -> anyhow::Result<Value> {
        let mut progress = ctx.meta.get_progress_token().map(Progress::new);
        let peer = ctx.peer.clone();
        let poll = self.client.poll_job(id, timeout, |j| {
            let note = progress.as_mut().and_then(|p| p.next(&j, Instant::now()));
            let peer = peer.clone();
            async move {
                if let Some(n) = note {
                    report(&peer, n).await;
                }
            }
        });
        tokio::select! {
            j = poll => j,
            _ = ctx.ct.cancelled() => anyhow::bail!("the call was cancelled; job {id} goes on in the daemon (`job_status` with id {id})"),
        }
    }

    /// Undo/redo: the daemon answers `{"revision": null}` when there is nothing to do.
    async fn step_history(&self, path: &str) -> Result<CallToolResult, McpError> {
        let v = self.client.post(path, &json!({})).await.map_err(err)?;
        if v["revision"].is_null() {
            return Err(McpError::invalid_request(format!("nothing to {}", path.rsplit('/').next().unwrap_or("undo")), None));
        }
        Ok(ok_json(&v))
    }
}

/// What a waiting tool tells a client about a job, when the call carried a progress token.
///
/// Worth sending for two reasons beyond a progress bar. Claude Code moves a tool call that
/// runs past 120 s to the background, where the latest of these is the task's status, so it
/// names the job an agent can pick up with `job_status`. And it abandons a stdio call that
/// has sent neither a result nor progress for 30 minutes, so a wait sends one at least every
/// `HEARTBEAT` even when the job has not moved.
struct Progress {
    token: ProgressToken,
    /// What was last sent: the progress value, and the message.
    sent: Option<(f64, String)>,
    at: Instant,
}

const HEARTBEAT: Duration = Duration::from_secs(20);

impl Progress {
    fn new(token: ProgressToken) -> Self {
        Progress { token, sent: None, at: Instant::now() }
    }

    /// The notification to send for job `j` polled at `now`, if one is due: the first poll,
    /// a change, or a heartbeat. Progress is the job's percent of 100, but MCP has it rise
    /// with every notification, and a job's percent can fall back when it moves on to its
    /// next stage (a sample bakes, then renders): such a notification, or a heartbeat,
    /// nudges the value up by a hair instead.
    fn next(&mut self, j: &Value, now: Instant) -> Option<ProgressNotificationParam> {
        let percent = j["percent"].as_f64().unwrap_or(0.0);
        let message = format!("job {} ({}), {}: {}", j["id"], j["kind"].as_str().unwrap_or("job"), j["state"].as_str().unwrap_or("?"), j["message"].as_str().unwrap_or(""));
        let value = match &self.sent {
            None => percent,
            Some((last, _)) if percent > *last => percent,
            Some((last, said)) if *said != message || now.duration_since(self.at) >= HEARTBEAT => last + 0.001,
            Some(_) => return None,
        };
        self.sent = Some((value, message.clone()));
        self.at = now;
        Some(ProgressNotificationParam::new(self.token.clone(), value).with_total(100.0).with_message(message))
    }
}

/// Best effort: a client that has gone away does not stop the wait, its cancel does.
async fn report(peer: &Peer<RoleServer>, n: ProgressNotificationParam) {
    if let Err(e) = peer.notify_progress(n).await {
        tracing::debug!("progress notification not sent: {e}");
    }
}

/// Every job without its result, which for a sample or a measurement is most of it.
fn job_summaries(all: &[Value]) -> Value {
    Value::Array(all.iter().map(|j| json!({ "id": j["id"], "kind": j["kind"], "state": j["state"], "percent": j["percent"], "message": j["message"], "params": j["params"] })).collect())
}

/// A job as a tool returns it, with how to keep waiting when it has not finished.
fn resumable(mut j: Value) -> Value {
    let running = !matches!(j["state"].as_str(), Some("done" | "failed" | "cancelled"));
    if let (true, Some(id)) = (running, j["id"].as_u64()) {
        j["next"] = json!(format!("still {}: call job_status with id {id} and wait_seconds (up to about 100) to keep waiting, or cancel_job", j["state"].as_str().unwrap_or("running")));
    }
    j
}

/// The daemon's `[{id, manifest}]` without what makes a library tens of thousands of tokens:
/// each var's default, range and description, leaving its type.
fn blueprint_summaries(all: &[Value]) -> Value {
    let summary = |b: &Value| {
        let m = &b["manifest"];
        let vars: serde_json::Map<String, Value> = m["vars"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v["type"].clone())).collect();
        let mut out = json!({ "id": b["id"], "name": m["name"], "description": m["description"], "tags": m["tags"], "vars": vars });
        if m["static"] == true {
            out["static"] = json!(true);
        }
        out
    };
    Value::Array(all.iter().map(summary).collect())
}

fn ok_json(v: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(v).unwrap_or_default())])
}

fn err(e: anyhow::Error) -> McpError {
    McpError::internal_error(format!("{e:#}"), None)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ApplyEdits {
    /// RFC 6902 JSON Patch operations against the project document.
    #[serde(default)]
    pub patch: Option<Vec<Value>>,
    /// Or a file inside the project holding the operations (a JSON array), e.g.
    /// `patches/diff.json`: the daemon reads it, so a large patch is applied exactly as a
    /// script wrote it, in one call. One of `patch` or `patch_path`.
    #[serde(default)]
    pub patch_path: Option<String>,
    /// The `revision` of the project you read. Rejected (409) if someone edited since.
    #[serde(default)]
    pub base_revision: Option<u64>,
    /// Short human-readable label for the undo history, e.g. "add callout at 12s".
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NewOverlay {
    /// Folder name under `overlays/`: `[A-Za-z_][A-Za-z0-9_-]*`.
    pub id: String,
    /// Display name; defaults to the id.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OverlayPath {
    /// The overlay folder or its `overlay.json`, e.g. `overlays/title`.
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ImportOverlay {
    /// The overlay folder or its `overlay.json`, e.g. `overlays/title`.
    pub path: String,
    /// Bin id; minted from the folder name when omitted.
    #[serde(default)]
    pub id: Option<String>,
    /// Display name; the manifest's `name` when omitted.
    #[serde(default)]
    pub name: Option<String>,
    /// Var overrides, checked against the manifest.
    #[serde(default)]
    pub vars: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub base_revision: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListBlueprints {
    /// A blueprint id: return its whole manifest rather than every blueprint's summary.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddBlueprint {
    /// Blueprint id, from `list_blueprints`.
    pub blueprint: String,
    /// Folder name for the copy under `overlays/` ([A-Za-z_][A-Za-z0-9_-]*), so it says
    /// what it holds (`kanji-title`, not `title-2`). The blueprint's id when omitted; a
    /// taken name gets a numeric suffix either way.
    #[serde(default)]
    pub folder: Option<String>,
    /// Bin id; minted from the folder name when omitted.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Var overrides, checked against the manifest.
    #[serde(default)]
    pub vars: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BinArg {
    /// Bin id of an overlay entry.
    pub bin: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SaveBlueprint {
    /// The overlay bin entry to save; its vars become the blueprint's defaults.
    pub bin: String,
    /// The blueprint's id (its folder name in the library).
    pub id: String,
    /// Replace an existing blueprint of that id; only when the user said so.
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SampleFrames {
    /// Start time, seconds.
    #[serde(default)]
    pub t0: f64,
    /// End time, seconds. Defaults to t0 (single frame).
    #[serde(default)]
    pub t1: Option<f64>,
    /// Number of evenly spaced frames in [t0, t1], 1..32. Default 1 for a single time and
    /// 5 across a range.
    #[serde(default)]
    pub count: Option<usize>,
    /// Frames at exactly these times (seconds, up to 32) instead of `t0`/`t1`/`count`:
    /// every cut and a moment either side, the key beat of each scene. Each bakes only the
    /// overlays it shows.
    #[serde(default)]
    pub times: Option<Vec<f64>>,
    /// Return the frames as one contact sheet, left to right then down, each captioned with
    /// its time, instead of one image per frame. Frames on it are 480 px wide unless `width` says.
    #[serde(default)]
    pub tile: Option<bool>,
    /// Columns of the sheet; about square when omitted.
    #[serde(default)]
    pub columns: Option<u32>,
    /// Crop rectangle in composition pixels `{x, y, w, h}`.
    #[serde(default)]
    pub rect: Option<Rect>,
    /// Output width in pixels (aspect kept). Default 960 for full frames.
    #[serde(default)]
    pub width: Option<u32>,
    /// Include baked overlays (default true). Set false for a faster footage-only look.
    #[serde(default)]
    pub overlays: Option<bool>,
    /// Bin id of the sequence to sample. The project's `master` when omitted.
    #[serde(default)]
    pub sequence: Option<String>,
    /// Bin id of an overlay **entry** to sample on its own, with no placement — the way to
    /// check an animation you are authoring before it is in the cut. `t0`/`t1` are then
    /// seconds into the overlay. Not combinable with `sequence`.
    #[serde(default)]
    pub overlay: Option<String>,
    /// With `overlay`: how long to run it for. The manifest's `duration_default` when
    /// omitted — an entry has no length until a clip gives it one.
    #[serde(default)]
    pub duration: Option<f64>,
    /// With `overlay`: the colour behind it (`#102030`), or `transparent` for straight
    /// alpha. Black by default — an overlay over nothing is not a picture of anything.
    #[serde(default)]
    pub backdrop: Option<String>,
    /// With `overlay`: `[width, height]` to render it into instead of the project's frame.
    /// An overlay must look right at 1920×1080 and 1080×1920 alike; this is how to check
    /// the other way round without editing the project.
    #[serde(default)]
    pub frame: Option<(u32, u32)>,
    /// How long to wait for the frames, seconds (default 90). A sample that is still
    /// baking the overlays it shows then returns its job id and progress instead.
    #[serde(default)]
    pub wait_seconds: Option<u64>,
    /// The job id of an earlier sample that was still baking: wait for it again and return
    /// its frames. Every other field is ignored.
    #[serde(default)]
    pub job: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PruneCache {
    /// Delete every bake and sampled frame, not only unused ones. Only when the user asks.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BakeOverlay {
    /// Clip ids to bake; all overlay clips when omitted.
    #[serde(default)]
    pub clips: Option<Vec<String>>,
    /// Seconds to wait before returning the job id instead (default 1800).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Render {
    /// Export preset id (see `list_presets`). Default `youtube-1080p`.
    #[serde(default)]
    pub preset: Option<String>,
    /// Output path, relative to the project directory. Defaults to `renders/<name>-<preset>.<ext>`.
    #[serde(default)]
    pub output: Option<String>,
    /// Optional `[start, end]` seconds to render only part of the timeline.
    #[serde(default)]
    pub range: Option<(f64, f64)>,
    /// Bin id of the sequence to render. The project's `master` when omitted.
    #[serde(default)]
    pub sequence: Option<String>,
    /// Frames rendered at once (1–64); the machine's default when omitted. The file is the
    /// same however many; fewer leaves the machine responsive.
    #[serde(default)]
    pub threads: Option<usize>,
    /// Rate factor within the preset's `quality` (see `list_presets`): lower is better and
    /// larger. The preset's own when omitted.
    #[serde(default)]
    pub quality: Option<u32>,
    /// One of the preset's `speed.options`: faster is larger at the same quality. The
    /// preset's own when omitted.
    #[serde(default)]
    pub speed: Option<String>,
    /// Seconds to wait for completion before returning the job id (default 900).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

impl Render {
    /// The `POST /api/render` body: what was given, the rest left to the daemon.
    fn body(&self) -> Value {
        json!({
            "preset": self.preset.clone().unwrap_or_else(|| "youtube-1080p".into()),
            "output": self.output, "range": self.range, "sequence": self.sequence,
            "threads": self.threads, "quality": self.quality, "speed": self.speed,
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MeasureAudio {
    /// Bin id of the sequence to measure. The project's `master` when omitted.
    #[serde(default)]
    pub sequence: Option<String>,
    /// Optional `[start, end]` seconds to measure only part of it.
    #[serde(default)]
    pub range: Option<(f64, f64)>,
    /// Parts to measure one by one, `{start, end, label}` in seconds. By default, every
    /// clip of the sequence that places another sequence (its chapters).
    #[serde(default)]
    pub sections: Option<Vec<Value>>,
    /// Seconds to wait before returning the job id instead (default 600).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddMedia {
    /// Media path relative to the project directory (or absolute).
    pub path: String,
    /// Bin id; defaults to a slug of the file name.
    #[serde(default)]
    pub id: Option<String>,
    /// Bin id of the entry (usually a folder) to list it under.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub base_revision: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Transcribe {
    /// Bin id of the media entry (video or audio) to transcribe: the result is added to the bin, unplaced.
    #[serde(default)]
    pub bin: Option<String>,
    /// Or a clip id: transcribe what it plays and place the subtitles over it, aligned. One of the two.
    #[serde(default)]
    pub clip: Option<String>,
    /// Bin id for the subtitles entry. Defaults to `<bin>-subs`.
    #[serde(default)]
    pub id: Option<String>,
    /// Overwrite the transcript of an entry that already exists (keeping its styling and
    /// its placement) instead of failing. Re-running otherwise refuses.
    #[serde(default)]
    pub replace: Option<bool>,
    /// ISO 639-1 language code, or `auto` (default).
    #[serde(default)]
    pub language: Option<String>,
    /// Longest cue in characters. Default 1: one cue per word, which the `subtitles` overlay groups into lines itself; 0 keeps whisper's own sentences.
    #[serde(default)]
    pub max_chars: Option<u32>,
    /// Seconds to wait for the model before returning the job id (default 600).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PathArg {
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddFont {
    /// A `.ttf` or `.otf` file, or a `.ttc`/`.otc` collection, relative to the project or absolute.
    pub path: String,
    /// Into the user's library for every project, instead of this project's `fonts/`.
    /// Only when the user asks: the library is theirs.
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JobArg {
    pub id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JobStatus {
    /// The job's id. Every job the daemon remembers, briefly, when omitted.
    #[serde(default)]
    pub id: Option<u64>,
    /// Wait up to this many seconds for the job to finish, then return it either way
    /// (default 0: return it as it is now). Waiting in calls of about 100 s keeps each one
    /// short of a client moving it to the background.
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

#[tool_router]
impl KumaServer {
    #[tool(description = "Read the whole project document (bin, tracks, clips, effects) including its current revision.")]
    async fn get_project(&self) -> Result<CallToolResult, McpError> {
        self.get("/api/project").await
    }

    #[tool(description = "JSON Schema for project files (`project`) and overlay manifests, `overlay.json` (`overlay`). Validate your edits against it.")]
    async fn get_schema(&self) -> Result<CallToolResult, McpError> {
        self.get("/api/schema").await
    }

    #[tool(description = "Apply a JSON Patch to the project: `patch` inline, or `patch_path`, a file in the project holding it (for a patch a script wrote, of any size). Atomic: rejected entirely if the result is invalid or the base_revision is stale. Returns the new revision, the number of operations applied and any warnings.")]
    async fn apply_edits(&self, Parameters(p): Parameters<ApplyEdits>) -> Result<CallToolResult, McpError> {
        let origin = format!("claude: {}", p.note.unwrap_or_else(|| "edit".into()));
        self.post("/api/project/patch", &json!({ "patch": p.patch, "patch_path": p.patch_path, "base_revision": p.base_revision, "origin": origin })).await
    }

    #[tool(description = "Undo the last edit (yours or the user's). Returns the new revision; fails when there is nothing to undo.")]
    async fn undo(&self) -> Result<CallToolResult, McpError> {
        self.step_history("/api/project/undo").await
    }

    #[tool(description = "Redo the last undone edit. Returns the new revision; fails when there is nothing to redo.")]
    async fn redo(&self) -> Result<CallToolResult, McpError> {
        self.step_history("/api/project/redo").await
    }

    #[tool(description = "Validate the project and list issues (errors and warnings) with JSON pointers.")]
    async fn validate_project(&self) -> Result<CallToolResult, McpError> {
        self.get("/api/project/validate").await
    }

    #[tool(description = "Rescan the user's library and list its blueprints (reusable overlays): id, name, description, tags and each var's type. With `id`, that blueprint's whole manifest: every var's default, range, options and description. Failed manifests are listed by `health`.")]
    async fn list_blueprints(&self, Parameters(p): Parameters<ListBlueprints>) -> Result<CallToolResult, McpError> {
        let all = self.client.post("/api/blueprints", &json!({})).await.map_err(err)?;
        let all = all.as_array().map(Vec::as_slice).unwrap_or_default();
        let Some(id) = p.id else {
            return Ok(ok_json(&blueprint_summaries(all)));
        };
        match all.iter().find(|b| b["id"] == id.as_str()) {
            Some(b) => Ok(ok_json(b)),
            None => Err(McpError::invalid_params(format!("no blueprint `{id}` in the library; `list_blueprints` without an id names them"), None)),
        }
    }

    #[tool(description = "Copy a blueprint into the project's `overlays/` and add it to the bin. Returns the bin id; the copy is the project's to edit.")]
    async fn add_blueprint(&self, Parameters(p): Parameters<AddBlueprint>) -> Result<CallToolResult, McpError> {
        let copied = self.client.post(&format!("/api/blueprints/{}/copy", p.blueprint), &json!({ "folder": p.folder })).await.map_err(err)?;
        let body = json!({ "path": copied["overlay"], "id": p.id, "name": p.name, "vars": p.vars.unwrap_or_default(), "origin": format!("claude: add {}", p.blueprint) });
        self.post("/api/overlays/import", &body).await
    }

    #[tool(description = "List the font families a `font` var can name, each with where it comes from (`project` fonts/ or the `library`), the writing systems it can set (`scripts`: latin, han, kana, hangul, …) and its faces (weight range, italic). A family not listed renders in the overlay's fallback stack, and so does text in a script the family lacks: that fallback is whatever the baking machine has, so pick (or add) a family that covers the text.")]
    async fn list_fonts(&self) -> Result<CallToolResult, McpError> {
        self.get("/api/fonts").await
    }

    #[tool(description = "Copy a font file (.ttf/.otf) into the project's `fonts/` (or, with `shared`, the user's library) so `font` vars can name it. A collection (.ttc/.otc, how most CJK fonts ship) is split into one file per face. Returns the family it reads as, and every face with its family and scripts. Not an edit: nothing to undo. Add a family's weights and italics as separate calls.")]
    async fn add_font(&self, Parameters(p): Parameters<AddFont>) -> Result<CallToolResult, McpError> {
        self.post("/api/fonts", &json!({ "path": p.path, "shared": p.shared })).await
    }

    #[tool(description = "How the timeline works: the bin and placements, effects and keyframes, nested sequences, sampling, overlays, subtitles, portrait. Read it before your first edit.")]
    async fn timeline_guide(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(TIMELINE_GUIDE)]))
    }

    #[tool(description = "The overlay authoring contract (root attributes, kuma.js helper, vars, seekable animation rules, manifest format). Read it before writing an overlay.")]
    async fn overlay_guide(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(include_str!("../../../library/blueprints/CLAUDE.md"))]))
    }

    #[tool(description = "List export presets (YouTube 1080p/4K, YouTube on the NVIDIA GPU, draft, VP9, ProRes); a 9:16 project renders vertical with any. Each has `quality` {min, max, default} and `speed` {options fastest first, default} that `render` may override (null where the codec has none), and `unavailable`: why this machine cannot use it, else null.")]
    async fn list_presets(&self) -> Result<CallToolResult, McpError> {
        self.get("/api/presets").await
    }

    #[tool(description = "Probe a media file with ffprobe: duration, size, fps, audio. Does not change the project.")]
    async fn probe_media(&self, Parameters(p): Parameters<PathArg>) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get_query("/api/media/probe", &[("path", &p.path)]).await.map_err(err)?))
    }

    #[tool(description = "Probe a media file and add it to the project bin (an overlay folder of the project is imported instead). Returns the bin id to reference from a clip.")]
    async fn add_media(&self, Parameters(p): Parameters<AddMedia>) -> Result<CallToolResult, McpError> {
        self.post("/api/bin", &json!({ "path": p.path, "id": p.id, "parent": p.parent, "base_revision": p.base_revision, "origin": "claude: add media" })).await
    }

    #[tool(description = "Transcribe media audio with a local whisper.cpp model into a `subtitles` overlay entry (cues in source seconds, editable as its `cues` var), listed under the media. Pass `clip` to place it over that clip, aligned, on an overlay track; pass `bin` for the entry alone. The new entry's id is minted when the words are in and returned as `bin`; it takes no base_revision, since the model runs for minutes and the patch is composed against the project as it is when it lands. A job still running after `wait_seconds` comes back with its id and `next`: `job_status` with that id and `wait_seconds` waits again, in calls short enough for your client (Claude Code moves a call past 120 s to the background, where it still finishes); `job_status` without an id lists jobs.")]
    async fn transcribe(&self, Parameters(p): Parameters<Transcribe>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let body = json!({ "bin": p.bin, "clip": p.clip, "id": p.id, "replace": p.replace.unwrap_or(false), "language": p.language, "max_chars": p.max_chars, "origin": "claude: transcribe" });
        self.job("/api/transcribe", &body, Duration::from_secs(p.wait_seconds.unwrap_or(600)), &ctx).await
    }

    #[tool(description = "Scaffold `overlays/<id>/`, a contract-abiding skeleton to edit, and return its lint report. Does not touch the bin: `import_overlay` it when ready. Refuses an existing id.")]
    async fn new_overlay(&self, Parameters(p): Parameters<NewOverlay>) -> Result<CallToolResult, McpError> {
        let body = json!({ "id": p.id, "name": p.name.unwrap_or_default(), "description": p.description.unwrap_or_default() });
        self.post("/api/overlays/new", &body).await
    }

    #[tool(description = "Check an overlay folder against the authoring contract (plus hyperframes' linter) without rendering. Run it after every edit: a contract break otherwise shows up as a 45 s bake timeout or a wrong frame.")]
    async fn check_overlay(&self, Parameters(p): Parameters<OverlayPath>) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get_query("/api/overlays/lint", &[("path", &p.path)]).await.map_err(err)?))
    }

    #[tool(description = "Add an overlay folder to the bin, optionally with vars; one outside the project is copied into `overlays/` first. Returns the bin id. Import one folder twice, with other vars, for a second wording.")]
    async fn import_overlay(&self, Parameters(p): Parameters<ImportOverlay>) -> Result<CallToolResult, McpError> {
        let body = json!({ "path": p.path, "id": p.id, "name": p.name, "vars": p.vars.unwrap_or_default(), "base_revision": p.base_revision, "origin": "claude: import overlay" });
        self.post("/api/overlays/import", &body).await
    }

    #[tool(description = "Give an overlay entry its own copy of the folder it shares, so editing it changes that entry alone. Returns the new folder.")]
    async fn make_overlay_unique(&self, Parameters(p): Parameters<BinArg>) -> Result<CallToolResult, McpError> {
        self.post("/api/overlays/unique", &json!({ "bin": p.bin, "origin": "claude: make overlay unique" })).await
    }

    #[tool(description = "Save an overlay entry into the user's library as a blueprint, its vars as the defaults. Only when the user asks. Refuses to replace one unless `force`.")]
    async fn save_blueprint(&self, Parameters(p): Parameters<SaveBlueprint>) -> Result<CallToolResult, McpError> {
        self.post("/api/overlays/save-blueprint", &json!({ "bin": p.bin, "id": p.id, "force": p.force.unwrap_or(false) })).await
    }

    #[tool(description = "Render exact composited frames (through the real MLT graph, overlays included) and return them as images: evenly over `t0`..`t1`, or at exactly the `times` given, and with `tile` as one captioned contact sheet (every cut ±0.3 s in one image). Use a rect to zoom into a region. Pass `sequence` to look at a nested sequence instead of the master, or `overlay` to look at one overlay bin entry on its own — no placement needed, times counted from the start of the overlay, which is how to check an animation you are authoring before it is in the cut. Frames are also written as PNGs whose paths are returned. An overlay changed since it was last baked is baked for its whole clip first, which can take a minute or more per overlay: after `wait_seconds` a sample still baking returns its job id and progress, and calling again with `job` set to that id picks it up.")]
    async fn sample_frames(&self, Parameters(p): Parameters<SampleFrames>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let id = match p.job {
            Some(id) => id,
            None => {
                let tile = p.tile.unwrap_or(false);
                let body = json!({
                    "t0": p.t0, "t1": p.t1, "count": p.count, "times": p.times, "tile": tile, "columns": p.columns,
                    "rect": p.rect.as_ref().map(|r| json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h })),
                    // A sheet sizes its own frames.
                    "width": p.width.or((p.rect.is_none() && !tile).then_some(960)),
                    "overlays": p.overlays.unwrap_or(true),
                    "sequence": p.sequence,
                    "overlay": p.overlay, "duration": p.duration, "backdrop": p.backdrop,
                    "frame_width": p.frame.map(|f| f.0), "frame_height": p.frame.map(|f| f.1),
                });
                let v = self.client.post("/api/sample/job", &body).await.map_err(err)?;
                v["job"].as_u64().ok_or_else(|| err(anyhow::anyhow!("daemon did not return a job id: {v}")))?
            }
        };
        let j = self.wait(id, Duration::from_secs(p.wait_seconds.unwrap_or(90)), &ctx).await.map_err(err)?;
        if j["kind"] != "sample" {
            return Err(McpError::invalid_params(format!("job {id} is a {} job, not a sample", j["kind"]), None));
        }
        match j["state"].as_str() {
            Some("done") => {}
            Some("failed") => return Err(err(anyhow::anyhow!("job {id} failed: {}", j["message"].as_str().unwrap_or("?")))),
            Some("cancelled") => return Err(McpError::internal_error(format!("sample job {id} was cancelled"), None)),
            _ => {
                let status = json!({
                    "job": id, "state": j["state"], "percent": j["percent"], "message": j["message"],
                    "next": format!("still baking: call sample_frames with job: {id} to keep waiting. Bakes are cached as each one finishes."),
                });
                return Ok(ok_json(&status));
            }
        }
        let mut content = Vec::new();
        let samples = j["result"]["samples"].as_array().cloned().unwrap_or_default();
        let listing: Vec<Value> = samples.iter().map(|s| json!({ "t": s["t"], "frame": s["frame"], "path": s["path"] })).collect();
        let sheet = j["result"]["sheet"].as_str();
        content.push(ContentBlock::text(serde_json::to_string_pretty(&json!({ "samples": listing, "sheet": sheet })).unwrap()));
        // The PNGs live on the daemon's disk; say so when this process cannot see them.
        let mut image = |label: String, path: &str| match std::fs::read(path) {
            Ok(bytes) => {
                content.push(ContentBlock::text(label));
                content.push(ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(bytes), "image/png"));
            }
            Err(e) => content.push(ContentBlock::text(format!("{label}: cannot read {path}: {e}"))),
        };
        match sheet {
            Some(path) => image(format!("contact sheet, {} frames in the order listed, left to right then down", samples.len()), path),
            None => {
                for s in &samples {
                    image(format!("t={:.3}s", s["t"].as_f64().unwrap_or(0.0)), s["path"].as_str().unwrap_or_default());
                }
            }
        }
        Ok(CallToolResult::success(content))
    }

    #[tool(description = "Bake overlay clips to alpha frame sequences now (normally done automatically at render). Cached by content; returns the bake resources. A job still running after `wait_seconds` comes back with its id and `next`: `job_status` with that id and `wait_seconds` waits again, in calls short enough for your client (Claude Code moves a call past 120 s to the background, where it still finishes); `job_status` without an id lists jobs.")]
    async fn bake_overlay(&self, Parameters(p): Parameters<BakeOverlay>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        self.job("/api/bake", &json!({ "clips": p.clips }), Duration::from_secs(p.wait_seconds.unwrap_or(1800)), &ctx).await
    }

    #[tool(description = "Render the timeline to a video file with an export preset. Renders the master unless `sequence` names another one. `threads`, `quality` and `speed` change this render only and default to the preset's (see list_presets). Renders run one at a time: a render asked for while another encodes waits `queued`. A job still running after `wait_seconds` comes back with its id and `next`: `job_status` with that id and `wait_seconds` waits again, in calls short enough for your client (Claude Code moves a call past 120 s to the background, where it still finishes); `job_status` without an id lists jobs. `cancel_job` stops it.")]
    async fn render(&self, Parameters(p): Parameters<Render>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        self.job("/api/render", &p.body(), Duration::from_secs(p.wait_seconds.unwrap_or(900)), &ctx).await
    }

    #[tool(description = "Measure how loud a sequence is (the master by default): its sound rendered through the real graph, no bakes needed, and metered to EBU R128. Returns integrated loudness (LUFS), loudness range and true peak (dBTP) overall and per section (by default each clip placing a sequence, i.e. each chapter), the five loudest moments, and silent stretches of a second or more. kuma has no limiter, so check this before a final render: a fade that silences music, or effects stacking on a peak, show up here. A job still running after `wait_seconds` comes back with its id and `next`: `job_status` with that id and `wait_seconds` waits again, in calls short enough for your client (Claude Code moves a call past 120 s to the background, where it still finishes); `job_status` without an id lists jobs.")]
    async fn measure_audio(&self, Parameters(p): Parameters<MeasureAudio>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let body = json!({ "sequence": p.sequence, "range": p.range, "sections": p.sections });
        self.job("/api/audio/measure", &body, Duration::from_secs(p.wait_seconds.unwrap_or(600)), &ctx).await
    }

    #[tool(description = "Delete what the project's `.kuma/` cache no longer uses: superseded bakes, sampled frames, thumbnails of removed entries. sample_frames leaves PNGs behind, so prune after a long session. Returns what was freed and what is left.")]
    async fn prune_cache(&self, Parameters(p): Parameters<PruneCache>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        self.job("/api/cache/prune", &json!({ "all": p.all }), Duration::from_secs(3600), &ctx).await
    }

    #[tool(description = "Status of a background job (render, bake, sample, measure, transcribe): state (queued, running, done, failed, cancelled), percent, message, result. With `wait_seconds`, waits up to that long for it to finish and returns it either way: the way to keep waiting on a long job in bounded calls. Without `id`, lists the jobs the daemon remembers, to find one whose call was moved to the background.")]
    async fn job_status(&self, Parameters(p): Parameters<JobStatus>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let Some(id) = p.id else {
            let all = self.client.get("/api/jobs").await.map_err(err)?;
            return Ok(ok_json(&job_summaries(all["jobs"].as_array().map(Vec::as_slice).unwrap_or_default())));
        };
        let j = self.wait(id, Duration::from_secs(p.wait_seconds.unwrap_or(0)), &ctx).await.map_err(err)?;
        Ok(ok_json(&resumable(j)))
    }

    #[tool(description = "Cancel a render job, whether it is queued or encoding, or an audio measurement. melt is stopped, the partial file is removed, and the job ends in state `cancelled`. Refused for a job that has finished or cannot be stopped.")]
    async fn cancel_job(&self, Parameters(p): Parameters<JobArg>) -> Result<CallToolResult, McpError> {
        self.post(&format!("/api/jobs/{}/cancel", p.id), &json!({})).await
    }

    #[tool(description = "Daemon health: project path and revision, hyperframes/browser/preview-engine availability, blueprint library dirs and any blueprint manifest that failed to load.")]
    async fn health(&self) -> Result<CallToolResult, McpError> {
        let mut h = self.client.get("/api/health").await.map_err(err)?;
        if let Some(obj) = h.as_object_mut() {
            if let Some(n) = &self.note {
                obj.insert("note".into(), Value::String(n.clone()));
            }
            if obj.get("browser").map(Value::is_null).unwrap_or(true) {
                obj.insert("browser_note".into(), Value::String("no Chrome headless shell configured; hyperframes downloads one on the first bake (or set HYPERFRAMES_BROWSER_PATH)".into()));
            }
        }
        Ok(ok_json(&h))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KumaServer {
    /// Every tool, through the router, and told when this server is an old build.
    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        let response = self.tool_router.call(ToolCallContext::new(self, request, context)).await;
        if self.build.replaced() { noted(response, REBUILT) } else { response }
    }

    fn get_info(&self) -> ServerInfo {
        let instructions = match &self.note {
            // First: a client keeps only the start of long instructions.
            Some(n) => format!("IMPORTANT — tell the user at the start: {n}\n\n{INSTRUCTIONS}"),
            None => INSTRUCTIONS.to_string(),
        };
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(instructions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code keeps the first 2048 characters of a server's instructions. Measured on
    /// what the client receives, with a note as long as the longest the CLI writes (the
    /// private-daemon one, which names the project twice).
    #[test]
    fn instructions_fit_the_client_limit() {
        const CLIENT_LIMIT: usize = 2048;
        let path = "/home/someone/videos/a-project-with-a-long-name/a-project-with-a-long-name.kuma.json";
        let note = format!("This MCP server is connected to a PRIVATE daemon for {path} because the daemon at http://127.0.0.1:7455 (the one behind the browser UI) serves a different project, {path}. Edits here will not show in that UI. To unify: run `kuma mcp-config --project {path}` and reconnect, or restart `kuma serve -p {path}`. Its own UI is at http://127.0.0.1:41234.");
        let s = KumaServer::new(Client::new("http://127.0.0.1:1"), Some(note));
        let n = s.get_info().instructions.unwrap().chars().count();
        assert!(n <= CLIENT_LIMIT, "instructions and note are {n} characters; move detail into timeline_guide.md");
    }

    /// The render options reach the daemon as given, and what is left out stays null for
    /// the daemon to fill in — the preset's own, or the machine's.
    #[test]
    fn render_passes_its_options_to_the_daemon() {
        let given: Render = serde_json::from_value(json!({ "preset": "draft", "threads": 2, "quality": 30, "speed": "veryfast" })).unwrap();
        let body = given.body();
        assert_eq!((&body["preset"], &body["threads"], &body["quality"], &body["speed"]), (&json!("draft"), &json!(2), &json!(30), &json!("veryfast")));
        let bare = serde_json::from_value::<Render>(json!({})).unwrap().body();
        assert_eq!(bare["preset"], "youtube-1080p");
        assert!(bare["threads"].is_null() && bare["quality"].is_null() && bare["speed"].is_null(), "{bare}");
    }

    /// The binary a server started from, replaced under it, is noticed, the way a rebuild
    /// replaces `target/release/kuma`; left alone, it is not.
    #[test]
    fn a_rebuilt_binary_is_noticed() {
        let path = std::env::temp_dir().join(format!("kuma-mcp-build-{}", std::process::id()));
        std::fs::write(&path, b"old build").unwrap();
        let build = Build::of(Some(path.clone()));
        assert!(!build.replaced());
        std::fs::write(&path, b"the new build").unwrap();
        assert!(build.replaced(), "a different size");
        std::fs::remove_file(&path).unwrap();
        assert!(build.replaced(), "gone, mid-build");
        assert!(!Build::of(None).replaced(), "an executable it cannot name is not a rebuild");
        assert!(!Build::current().replaced(), "the test binary is the one running");
    }

    /// A summary keeps what picks a blueprint and drops what configures one.
    #[test]
    fn a_blueprint_summary_names_its_vars_by_type() {
        let all = json!([{ "id": "title", "manifest": {
            "name": "Title", "description": "A big title", "duration_default": 4.0, "static": false, "tags": ["text"],
            "vars": { "text": { "type": "string", "default": "Hello", "description": "What it says" },
                      "accent": { "type": "color", "default": "#ff0000" } } } },
            { "id": "logo", "manifest": { "name": "Logo", "description": "", "duration_default": 4.0, "static": true, "tags": [], "vars": {} } }]);
        assert_eq!(blueprint_summaries(all.as_array().unwrap()), json!([
            { "id": "title", "name": "Title", "description": "A big title", "tags": ["text"], "vars": { "text": "string", "accent": "color" } },
            { "id": "logo", "name": "Logo", "description": "", "tags": [], "vars": {}, "static": true },
        ]));
    }

    /// The note goes first in a result, and into an error's message, so it is read whatever
    /// the tool said.
    #[test]
    fn a_stale_server_says_so_before_the_answer() {
        let ok: Result<CallToolResponse, McpError> = Ok(CallToolResult::success(vec![ContentBlock::text("answer")]).into());
        let Ok(CallToolResponse::Complete(r)) = noted(ok, REBUILT) else { panic!("still a result") };
        let texts: Vec<String> = r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect();
        assert_eq!(texts, [REBUILT, "answer"]);
        let Err(e) = noted(Err(McpError::invalid_params("bad", None)), REBUILT) else { panic!("still an error") };
        assert!(e.message.starts_with("kuma was rebuilt") && e.message.ends_with("bad"), "{}", e.message);
    }

    /// A wait reports the job's percent as it moves, nothing while it does not, and a
    /// heartbeat once it has been quiet for a while — always rising, as MCP asks, even when
    /// the job's own percent falls back for its next stage.
    #[test]
    fn a_wait_reports_progress_that_only_rises() {
        let job = |percent: u8, message: &str| json!({ "id": 7, "kind": "render", "state": "running", "percent": percent, "message": message });
        let t0 = Instant::now();
        let mut p = Progress::new(ProgressToken(rmcp::model::NumberOrString::Number(1)));
        let first = p.next(&job(10, "encoding"), t0).expect("the first poll is reported");
        assert_eq!((first.progress, first.total), (10.0, Some(100.0)));
        assert!(first.message.as_deref().unwrap().starts_with("job 7 (render), running"), "names the job to resume: {:?}", first.message);
        assert!(p.next(&job(10, "encoding"), t0 + Duration::from_secs(1)).is_none(), "nothing new, nothing sent");
        assert_eq!(p.next(&job(40, "encoding"), t0 + Duration::from_secs(2)).unwrap().progress, 40.0);
        let back = p.next(&job(5, "baking title"), t0 + Duration::from_secs(3)).expect("a new stage is news");
        assert!(back.progress > 40.0, "{}", back.progress);
        assert!(p.next(&job(5, "baking title"), t0 + Duration::from_secs(4)).is_none());
        let beat = p.next(&job(5, "baking title"), t0 + Duration::from_secs(3) + HEARTBEAT).expect("a heartbeat");
        assert!(beat.progress > back.progress && beat.progress < 41.0, "{}", beat.progress);
    }

    /// A job that has not finished says how to keep waiting; one that has, does not.
    #[test]
    fn an_unfinished_job_says_how_to_resume() {
        let running = resumable(json!({ "id": 3, "state": "queued" }));
        assert!(running["next"].as_str().unwrap().contains("job_status with id 3"), "{running}");
        assert!(resumable(json!({ "id": 3, "state": "done" })).get("next").is_none());
    }

    #[test]
    fn the_note_comes_before_the_instructions() {
        let s = KumaServer::new(Client::new("http://127.0.0.1:1"), Some("attached".into()));
        let text = s.get_info().instructions.unwrap();
        assert!(text.starts_with("IMPORTANT"), "{text}");
    }
}
