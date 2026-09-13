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

Overlays are HTML templates (`list_templates`): a clip on an `overlay` track references a template id
and `vars`. New templates are folders under `overlays/<id>/` with `index.html` + `template.json`; read
`overlays/CLAUDE.md` in the repository before authoring one. Overlays are live DOM in the editor's
preview and are baked to alpha frames at export.

To look at the footage, call `sample_frames` with a time range (and an optional pixel `rect`); it
returns the composited frames as images plus their PNG paths.
"#;

#[derive(Clone)]
pub struct KumaServer {
    client: Client,
    tool_router: ToolRouter<Self>,
}

impl KumaServer {
    pub fn new(client: Client) -> Self {
        Self { client, tool_router: Self::tool_router() }
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
pub struct SetProject {
    /// The whole project document.
    pub project: Value,
    #[serde(default)]
    pub base_revision: Option<u64>,
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
    /// 2 renders at double DPR and downscales for crisper edges.
    #[serde(default)]
    pub supersample: Option<u32>,
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
    #[serde(default)]
    pub supersample: Option<u32>,
    /// Seconds to wait for completion before returning the job id (default 900).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddAsset {
    /// Media path relative to the project directory (or absolute).
    pub path: String,
    /// Asset id; defaults to a slug of the file name.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub base_revision: Option<u64>,
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
    #[tool(description = "Read the whole project document (tracks, clips, effects, assets) including its current revision.")]
    async fn get_project(&self) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get("/api/project").await.map_err(err)?))
    }

    #[tool(description = "JSON Schema for project files and overlay template manifests. Validate your edits against it.")]
    async fn get_schema(&self) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get("/api/schema").await.map_err(err)?))
    }

    #[tool(description = "Apply a JSON Patch to the project. Atomic: rejected entirely if the result is invalid or the base_revision is stale. Returns the new revision and any warnings.")]
    async fn apply_edits(&self, Parameters(p): Parameters<ApplyEdits>) -> Result<CallToolResult, McpError> {
        let origin = format!("claude: {}", p.note.unwrap_or_else(|| "edit".into()));
        let body = json!({ "patch": p.patch, "base_revision": p.base_revision, "origin": origin });
        Ok(ok_json(&self.client.post("/api/project/patch", &body).await.map_err(err)?))
    }

    #[tool(description = "Replace the whole project document. Prefer apply_edits for incremental changes.")]
    async fn set_project(&self, Parameters(p): Parameters<SetProject>) -> Result<CallToolResult, McpError> {
        let origin = format!("claude: {}", p.note.unwrap_or_else(|| "replace project".into()));
        let body = json!({ "project": p.project, "base_revision": p.base_revision, "origin": origin });
        Ok(ok_json(&self.client.put("/api/project", &body).await.map_err(err)?))
    }

    #[tool(description = "Undo the last edit (yours or the user's). Returns the new revision.")]
    async fn undo(&self) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.post("/api/project/undo", &json!({})).await.map_err(err)?))
    }

    #[tool(description = "Redo the last undone edit.")]
    async fn redo(&self) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.post("/api/project/redo", &json!({})).await.map_err(err)?))
    }

    #[tool(description = "Validate the project and list issues (errors and warnings) with JSON pointers.")]
    async fn validate_project(&self) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get("/api/project/validate").await.map_err(err)?))
    }

    #[tool(description = "List overlay templates with their manifests (typed vars, defaults, default duration).")]
    async fn list_templates(&self) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.post("/api/templates", &json!({})).await.map_err(err)?))
    }

    #[tool(description = "List export presets (YouTube 1080p/4K/Shorts, draft, VP9, ProRes).")]
    async fn list_presets(&self) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get("/api/presets").await.map_err(err)?))
    }

    #[tool(description = "Probe a media file with ffprobe: duration, size, fps, audio, codec.")]
    async fn probe_asset(&self, Parameters(p): Parameters<PathArg>) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get(&format!("/api/assets/probe?path={}", urlencode(&p.path))).await.map_err(err)?))
    }

    #[tool(description = "Probe a media file and register it as a project asset. Returns the asset id to use in clips.")]
    async fn add_asset(&self, Parameters(p): Parameters<AddAsset>) -> Result<CallToolResult, McpError> {
        let body = json!({ "path": p.path, "id": p.id, "base_revision": p.base_revision, "origin": "claude: add asset" });
        Ok(ok_json(&self.client.post("/api/assets", &body).await.map_err(err)?))
    }

    #[tool(description = "Render exact composited frames of the timeline (through the real MLT graph, overlays included) and return them as images. Use a rect to zoom into a region. Frames are also written as PNGs whose paths are returned.")]
    async fn sample_frames(&self, Parameters(p): Parameters<SampleFrames>) -> Result<CallToolResult, McpError> {
        let body = json!({
            "t0": p.t0, "t1": p.t1, "count": p.count.unwrap_or(1),
            "rect": p.rect.as_ref().map(|r| json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h })),
            "width": p.width.or(if p.rect.is_some() { None } else { Some(960) }),
            "overlays": p.overlays.unwrap_or(true),
        });
        let v = self.client.post("/api/sample", &body).await.map_err(err)?;
        let mut content = Vec::new();
        let samples = v["samples"].as_array().cloned().unwrap_or_default();
        let listing: Vec<Value> = samples.iter().map(|s| json!({ "t": s["t"], "frame": s["frame"], "path": s["path"] })).collect();
        content.push(ContentBlock::text(serde_json::to_string_pretty(&json!({ "samples": listing })).unwrap()));
        for s in &samples {
            if let Some(path) = s["path"].as_str() {
                if let Ok(bytes) = std::fs::read(path) {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                    content.push(ContentBlock::text(format!("t={:.3}s", s["t"].as_f64().unwrap_or(0.0))));
                    content.push(ContentBlock::image(b64, "image/png"));
                }
            }
        }
        Ok(CallToolResult::success(content))
    }

    #[tool(description = "Bake overlay clips to alpha frame sequences now (normally done automatically at render). Cached by content; returns the bake resources.")]
    async fn bake_overlay(&self, Parameters(p): Parameters<BakeOverlay>) -> Result<CallToolResult, McpError> {
        let v = self.client.post("/api/bake", &json!({ "clips": p.clips, "supersample": p.supersample.unwrap_or(1) })).await.map_err(err)?;
        let id = v["job"].as_u64().unwrap_or(0);
        Ok(ok_json(&self.client.wait_job(id, Duration::from_secs(1800)).await.map_err(err)?))
    }

    #[tool(description = "Render the timeline to a video file with an export preset. Waits up to wait_seconds; if still running, returns the job id to poll with job_status.")]
    async fn render(&self, Parameters(p): Parameters<Render>) -> Result<CallToolResult, McpError> {
        let body = json!({
            "preset": p.preset.unwrap_or_else(|| "youtube-1080p".into()),
            "output": p.output, "range": p.range, "supersample": p.supersample.unwrap_or(1),
        });
        let v = self.client.post("/api/render", &body).await.map_err(err)?;
        let id = v["job"].as_u64().unwrap_or(0);
        Ok(ok_json(&self.client.wait_job(id, Duration::from_secs(p.wait_seconds.unwrap_or(900))).await.map_err(err)?))
    }

    #[tool(description = "Status of a background job (render/bake).")]
    async fn job_status(&self, Parameters(p): Parameters<JobArg>) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get(&format!("/api/jobs/{}", p.id)).await.map_err(err)?))
    }

    #[tool(description = "Daemon health: tool availability (melt, ffprobe, hyperframes, browser, preview engine) and project path.")]
    async fn health(&self) -> Result<CallToolResult, McpError> {
        Ok(ok_json(&self.client.get("/api/health").await.map_err(err)?))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KumaServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(INSTRUCTIONS)
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
