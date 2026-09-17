//! MCP tools. Each tool is a thin call into the daemon API; the interesting one is
//! `sample_frames`, which returns rendered frames as image content so the agent can look
//! at the actual composite.

use std::time::Duration;

use base64::Engine as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::Client;

const INSTRUCTIONS: &str = r#"kuma-editor: a hybrid video editor. The timeline is a JSON project (get it with `get_project`, its
JSON Schema with `get_schema`). Times are seconds (f64); clip `end` is exclusive; `fps` is rational.
Tracks composite bottom-up. Edit with `apply_edits` (RFC 6902 JSON Patch) and pass the `base_revision`
you read, so you never overwrite a human edit made meanwhile. Every edit is validated atomically and
undoable; label your edits with `note`.

Everything placeable lives in `bin`, keyed by a short id: media files (`{"source": {"path": …}}`),
configured overlays (`{"source": {"template": …, "vars": {…}}}`), colours, and *sequences*
(`{"source": {"tracks": [...], "markers": [...]}}`). A clip is a *placement* of one entry: it carries
`bin`, `start`, `end`, `in`, `speed` and `effects`, never a source of its own. So changing an
overlay's text edits its bin entry and every placement follows; to vary the text, add a second entry.
Add media with `add_media` rather than by hand.

**There is no project-level `tracks`.** A timeline is a sequence entry in the bin, and `master` names
the one that gets rendered — so a clip lives at `/bin/<sequence>/source/tracks/<i>/clips/<j>`. A
sequence placed on another sequence is a nested sequence: edit a part separately, then place it in
the final cut. `in` offsets into it and trimming works as on any clip; two placements of one sequence
share its content and keep their own effects. A sequence cannot contain itself (refused), cannot be
sped up (MLT cannot time-warp a timeline: render it out and place the result as media), and must not
be empty when placed. Track and clip ids are unique across the *whole project*, not per sequence.
`sample_frames` and `render` take an optional `sequence` and default to the master.

Overlay entries reference an HTML template (`list_templates`). Templates live in the user's library
(`~/.local/share/kuma/overlays/<id>/`) or in the project's own `overlays/<id>/`, as a folder with
`index.html` + `template.json`; call `template_guide` before authoring one. Overlays are live DOM in
the editor's preview and are baked to alpha frames at export.

To look at the footage, call `sample_frames` with a time range (and an optional pixel `rect`); it
returns the composited frames as images plus their PNG paths.

Subtitles: `transcribe` runs a local speech-to-text model (whisper.cpp) and adds a `subtitles`
overlay entry whose `cues` var is `[{start, end, text}, …]` in source seconds, one cue per word (the
template groups them by its `words_per_line` var and can stress the word being said), listed under the media
(`parent`). Given a `clip`, it also places the subtitles over that clip, aligned; given a `bin`, the
entry is yours to place (set its `offset` var to a trimmed clip's `in`). Fix a misheard word by
patching the cue's `text`. `parent` on a bin entry is organisation only: the entry it is listed under
in the browser, which can be any entry or a folder (`{"source": {"folder": true}}`, never placeable);
`order` sorts siblings there (ordered ones first, the rest by id).
Vertical (9:16) is only
`width`/`height`: patch them to 1080×1920 (or swap them) and every preset renders portrait.
"#;

#[derive(Clone)]
pub struct KumaServer {
    client: Client,
    note: Option<String>,
    tool_router: ToolRouter<Self>,
}

impl KumaServer {
    pub fn new(client: Client, note: Option<String>) -> Self {
        Self { client, note, tool_router: Self::tool_router() }
    }

    async fn get(&self, path: &str) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get(path).await.map_err(err)?))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.post(path, body).await.map_err(err)?))
    }

    /// Start a background job and wait for it (up to `timeout`).
    async fn job(&self, path: &str, body: &Value, timeout: Duration) -> Result<CallToolResult, McpError> {
        let v = self.client.post(path, body).await.map_err(err)?;
        let id = v["job"].as_u64().ok_or_else(|| err(anyhow::anyhow!("daemon did not return a job id: {v}")))?;
        Ok(ok_json(&self.client.wait_job(id, timeout).await.map_err(err)?))
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

fn ok_json(v: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(v).unwrap_or_default())])
}

fn err(e: anyhow::Error) -> McpError {
    McpError::internal_error(format!("{e:#}"), None)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ApplyEdits {
    /// RFC 6902 JSON Patch operations against the project document.
    pub patch: Vec<Value>,
    /// The `revision` of the project you read. Rejected (409) if someone edited since.
    #[serde(default)]
    pub base_revision: Option<u64>,
    /// Short human-readable label for the undo history, e.g. "add callout at 12s".
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SampleFrames {
    /// Start time, seconds.
    pub t0: f64,
    /// End time, seconds. Defaults to t0 (single frame).
    #[serde(default)]
    pub t1: Option<f64>,
    /// Number of evenly spaced frames in [t0, t1]. 1..32, default 1.
    #[serde(default)]
    pub count: Option<usize>,
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
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BakeOverlay {
    /// Clip ids to bake; all overlay clips when omitted.
    #[serde(default)]
    pub clips: Option<Vec<String>>,
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
    /// Seconds to wait for completion before returning the job id (default 900).
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
    /// Bin id for the new subtitles entry. Defaults to `<bin>-subs`.
    #[serde(default)]
    pub id: Option<String>,
    /// ISO 639-1 language code, or `auto` (default).
    #[serde(default)]
    pub language: Option<String>,
    /// Longest cue in characters. Default 1: one cue per word, which the `subtitles` template groups into lines itself; 0 keeps whisper's own sentences.
    #[serde(default)]
    pub max_chars: Option<u32>,
    #[serde(default)]
    pub base_revision: Option<u64>,
    /// Seconds to wait for the model before returning the job id (default 600).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PathArg {
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JobArg {
    pub id: u64,
}

#[tool_router]
impl KumaServer {
    #[tool(description = "Read the whole project document (bin, tracks, clips, effects) including its current revision.")]
    async fn get_project(&self) -> Result<CallToolResult, McpError> {
        self.get("/api/project").await
    }

    #[tool(description = "JSON Schema for project files and overlay template manifests. Validate your edits against it.")]
    async fn get_schema(&self) -> Result<CallToolResult, McpError> {
        self.get("/api/schema").await
    }

    #[tool(description = "Apply a JSON Patch to the project. Atomic: rejected entirely if the result is invalid or the base_revision is stale. Returns the new revision and any warnings.")]
    async fn apply_edits(&self, Parameters(p): Parameters<ApplyEdits>) -> Result<CallToolResult, McpError> {
        let origin = format!("claude: {}", p.note.unwrap_or_else(|| "edit".into()));
        self.post("/api/project/patch", &json!({ "patch": p.patch, "base_revision": p.base_revision, "origin": origin })).await
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

    #[tool(description = "Rescan the template libraries and list the templates with their manifests (typed vars, defaults, default duration). A template folder you just wrote appears here without a restart; manifests that failed to load are listed by `health`.")]
    async fn list_templates(&self) -> Result<CallToolResult, McpError> {
        self.post("/api/templates", &json!({})).await
    }

    #[tool(description = "The overlay template authoring contract (root attributes, kuma.js helper, vars, seekable animation rules, manifest format). Read it before writing a template.")]
    async fn template_guide(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(include_str!("../../../library/overlays/CLAUDE.md"))]))
    }

    #[tool(description = "List export presets (YouTube 1080p/4K, draft, VP9, ProRes). A 9:16 project renders vertical with the same presets.")]
    async fn list_presets(&self) -> Result<CallToolResult, McpError> {
        self.get("/api/presets").await
    }

    #[tool(description = "Probe a media file with ffprobe: duration, size, fps, audio. Does not change the project.")]
    async fn probe_media(&self, Parameters(p): Parameters<PathArg>) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get_query("/api/media/probe", &[("path", &p.path)]).await.map_err(err)?))
    }

    #[tool(description = "Probe a media file and add it to the project bin. Returns the bin id to reference from a clip.")]
    async fn add_media(&self, Parameters(p): Parameters<AddMedia>) -> Result<CallToolResult, McpError> {
        self.post("/api/bin", &json!({ "path": p.path, "id": p.id, "base_revision": p.base_revision, "origin": "claude: add media" })).await
    }

    #[tool(description = "Transcribe media audio with a local whisper.cpp model into a `subtitles` overlay entry (cues in source seconds, editable as its `cues` var), listed under the media. Pass `clip` to place it over that clip, aligned, on an overlay track; pass `bin` for the entry alone.")]
    async fn transcribe(&self, Parameters(p): Parameters<Transcribe>) -> Result<CallToolResult, McpError> {
        let body = json!({ "bin": p.bin, "clip": p.clip, "id": p.id, "language": p.language, "max_chars": p.max_chars, "base_revision": p.base_revision, "origin": "claude: transcribe" });
        self.job("/api/transcribe", &body, Duration::from_secs(p.wait_seconds.unwrap_or(600))).await
    }

    #[tool(description = "Render exact composited frames of the timeline (through the real MLT graph, overlays included) and return them as images. Use a rect to zoom into a region. Pass `sequence` to look at a nested sequence instead of the master. Frames are also written as PNGs whose paths are returned.")]
    async fn sample_frames(&self, Parameters(p): Parameters<SampleFrames>) -> Result<CallToolResult, McpError> {
        let body = json!({
            "t0": p.t0, "t1": p.t1, "count": p.count.unwrap_or(1),
            "rect": p.rect.as_ref().map(|r| json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h })),
            "width": p.width.or(if p.rect.is_some() { None } else { Some(960) }),
            "overlays": p.overlays.unwrap_or(true),
            "sequence": p.sequence,
        });
        let v = self.client.post("/api/sample", &body).await.map_err(err)?;
        let mut content = Vec::new();
        let samples = v["samples"].as_array().cloned().unwrap_or_default();
        let listing: Vec<Value> = samples.iter().map(|s| json!({ "t": s["t"], "frame": s["frame"], "path": s["path"] })).collect();
        content.push(ContentBlock::text(serde_json::to_string_pretty(&json!({ "samples": listing })).unwrap()));
        for s in &samples {
            let path = s["path"].as_str().unwrap_or_default();
            let label = format!("t={:.3}s", s["t"].as_f64().unwrap_or(0.0));
            // The PNGs live on the daemon's disk; say so when this process cannot see them.
            match std::fs::read(path) {
                Ok(bytes) => {
                    content.push(ContentBlock::text(label));
                    content.push(ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(bytes), "image/png"));
                }
                Err(e) => content.push(ContentBlock::text(format!("{label}: cannot read {path}: {e}"))),
            }
        }
        Ok(CallToolResult::success(content))
    }

    #[tool(description = "Bake overlay clips to alpha frame sequences now (normally done automatically at render). Cached by content; returns the bake resources.")]
    async fn bake_overlay(&self, Parameters(p): Parameters<BakeOverlay>) -> Result<CallToolResult, McpError> {
        self.job("/api/bake", &json!({ "clips": p.clips }), Duration::from_secs(1800)).await
    }

    #[tool(description = "Render the timeline to a video file with an export preset. Renders the master unless `sequence` names another one. Waits up to wait_seconds; if still running, returns the job id to poll with job_status.")]
    async fn render(&self, Parameters(p): Parameters<Render>) -> Result<CallToolResult, McpError> {
        let body = json!({
            "preset": p.preset.unwrap_or_else(|| "youtube-1080p".into()),
            "output": p.output, "range": p.range, "sequence": p.sequence,
        });
        self.job("/api/render", &body, Duration::from_secs(p.wait_seconds.unwrap_or(900))).await
    }

    #[tool(description = "Status of a background job (render/bake): percent, message, done, ok, result.")]
    async fn job_status(&self, Parameters(p): Parameters<JobArg>) -> Result<CallToolResult, McpError> {
        self.get(&format!("/api/jobs/{}", p.id)).await
    }

    #[tool(description = "Daemon health: project path and revision, hyperframes/browser/preview-engine availability, template library dirs and any manifest that failed to load.")]
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
    fn get_info(&self) -> ServerInfo {
        let instructions = match &self.note {
            Some(n) => format!("{INSTRUCTIONS}\nIMPORTANT — tell the user at the start: {n}\n"),
            None => INSTRUCTIONS.to_string(),
        };
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(instructions)
    }
}
