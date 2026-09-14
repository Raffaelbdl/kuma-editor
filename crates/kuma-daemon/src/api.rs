//! HTTP + WebSocket API. Everything the UI and the MCP server do goes through here.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kuma_core::edit::EditError;
use kuma_core::{validate, ClipSource, Project};
use kuma_mlt::UnbakedPolicy;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;

use crate::jobs::{self, RenderRequest, SampleRequest};
use crate::preview;
use crate::state::AppState;

type S = State<Arc<AppState>>;

pub fn router(state: Arc<AppState>) -> Router {
    let mut app = Router::new()
        .route("/api/health", get(health))
        .route("/api/project", get(get_project).put(put_project))
        .route("/api/project/patch", post(patch_project))
        .route("/api/project/undo", post(undo))
        .route("/api/project/redo", post(redo))
        .route("/api/project/history", get(history))
        .route("/api/project/validate", get(validate_project))
        .route("/api/schema", get(schema))
        .route("/api/templates", get(templates).post(reload_templates))
        .route("/api/templates/thumbs", post(thumbs))
        .route("/api/presets/effects", get(effect_presets))
        .route("/api/assets/probe", get(probe_asset))
        .route("/api/assets", post(add_asset))
        .route("/api/presets", get(presets))
        .route("/api/render", post(render))
        .route("/api/bake", post(bake))
        .route("/api/sample", post(sample))
        .route("/api/compile", get(compile_xml))
        .route("/api/snapshot", get(snapshot))
        .route("/api/jobs", get(jobs_list))
        .route("/api/jobs/{id}", get(job_get))
        .route("/api/events", get(events_ws))
        .route("/api/preview/ws", get(preview_ws))
        .route("/api/preview/reload", post(preview_reload))
        .route("/overlay-preview/{clip}", get(overlay_preview))
        .route("/overlay-preview/{clip}/{*rest}", get(overlay_file))
        .route("/overlays/{id}/{*rest}", get(overlay_static))
        .route("/vendor/{*rest}", get(vendor))
        .route("/file", get(file));
    if let Some(ui) = &state.config.ui_dir {
        if ui.is_dir() {
            app = app.fallback_service(ServeDir::new(ui).append_index_html_on_directories(true));
        }
    }
    app.with_state(state)
}

// ---------- errors ----------

struct ApiError(StatusCode, Value);

impl ApiError {
    fn new(status: StatusCode, msg: impl std::fmt::Display) -> Self {
        ApiError(status, json!({ "error": msg.to_string() }))
    }
    fn bad_request(msg: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
    fn not_found(msg: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::NOT_FOUND, msg)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl From<EditError> for ApiError {
    fn from(e: EditError) -> Self {
        match e {
            EditError::RevisionMismatch { current, base } => {
                ApiError(StatusCode::CONFLICT, json!({ "error": e.to_string(), "current_revision": current, "base_revision": base }))
            }
            EditError::Invalid(issues) => ApiError(StatusCode::UNPROCESSABLE_ENTITY, json!({ "error": "validation failed", "issues": issues })),
            other => Self::bad_request(other),
        }
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

/// Run CPU/subprocess work off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(|e| anyhow::anyhow!(e).into())
}

/// `base/rest` for a client-supplied relative path, or `None` when it is absolute or
/// climbs out of `base`.
fn safe_join(base: &std::path::Path, rest: &str) -> Option<PathBuf> {
    use std::path::Component;
    let rel = std::path::Path::new(rest);
    rel.components().all(|c| matches!(c, Component::Normal(_))).then(|| base.join(rel))
}

// ---------- basics ----------

async fn health(State(st): S) -> Json<Value> {
    let p = st.project.read().unwrap();
    Json(json!({
        "ok": true,
        "project": st.config.project_file,
        "revision": p.revision,
        "melt": kuma_mlt::melt::melt_available(),
        "ffprobe": kuma_overlay::media::ffprobe_available(),
        "hyperframes": st.baker.version,
        "browser": st.baker.browser,
        "preview_engine": crate::preview::PreviewHandle::engine_path(&st).is_file(),
        "preview_engine_path": crate::preview::PreviewHandle::engine_path(&st),
        "preview_running": st.preview.get().is_some(),
        "preview_error": st.preview.last_error.lock().unwrap().clone(),
        "preview_scale": *st.preview.scale.lock().unwrap(),
    }))
}

async fn get_project(State(st): S) -> Json<Project> {
    Json(st.project.read().unwrap().clone())
}

#[derive(Deserialize)]
struct PutProject {
    project: Value,
    #[serde(default)]
    base_revision: Option<u64>,
    #[serde(default = "default_origin")]
    origin: String,
}
fn default_origin() -> String {
    "ui".into()
}

async fn put_project(State(st): S, Json(req): Json<PutProject>) -> ApiResult {
    let (rev, warnings) = st.replace_project(req.project, req.base_revision, &req.origin)?;
    Ok(Json(json!({ "revision": rev, "warnings": warnings })))
}

#[derive(Deserialize)]
struct PatchProject {
    patch: json_patch::Patch,
    #[serde(default)]
    base_revision: Option<u64>,
    #[serde(default = "default_origin")]
    origin: String,
}

async fn patch_project(State(st): S, Json(req): Json<PatchProject>) -> ApiResult {
    let (rev, warnings) = st.apply_patch(&req.patch.0, req.base_revision, &req.origin)?;
    Ok(Json(json!({ "revision": rev, "warnings": warnings })))
}

async fn undo(State(st): S) -> ApiResult {
    Ok(Json(json!({ "revision": st.undo() })))
}
async fn redo(State(st): S) -> ApiResult {
    Ok(Json(json!({ "revision": st.redo() })))
}
async fn history(State(st): S) -> Json<Value> {
    let (past, future) = st.history();
    Json(json!({ "undo": past, "redo": future }))
}

async fn validate_project(State(st): S) -> Json<Value> {
    let p = st.project.read().unwrap().clone();
    let issues = validate(&p, &st.validation_context());
    Json(json!({ "issues": issues, "duration": p.duration() }))
}

async fn schema() -> Json<Value> {
    Json(json!({
        "project": kuma_core::schema::project_schema(),
        "template": kuma_core::schema::template_schema(),
    }))
}

async fn templates(State(st): S) -> Json<Value> {
    Json(json!(st.template_infos()))
}

async fn reload_templates(State(st): S) -> Json<Value> {
    st.reload_templates();
    Json(json!(st.template_infos()))
}

async fn presets() -> Json<Value> {
    Json(json!(kuma_mlt::PRESETS))
}

/// Effect presets: `presets/effects/*.json` from the repo, then the project (wins on id).
async fn effect_presets(State(st): S) -> Json<Value> {
    let mut out: std::collections::BTreeMap<String, Value> = Default::default();
    for dir in [st.config.repo_root.join("presets/effects"), st.config.project_root().join("presets/effects")] {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "json").unwrap_or(false) {
                if let Ok(v) = std::fs::read_to_string(&p).map_err(|_| ()).and_then(|s| serde_json::from_str::<Value>(&s).map_err(|_| ())) {
                    if let Some(id) = v.get("id").and_then(Value::as_str) {
                        out.insert(id.to_string(), v);
                    }
                }
            }
        }
    }
    Json(json!(out.into_values().collect::<Vec<_>>()))
}

/// Generate `thumb.png` for every template (or the given ids) as a background job.
async fn thumbs(State(st): S, Json(req): Json<ThumbsReq>) -> Json<Value> {
    let s2 = st.clone();
    let id = jobs::spawn(st, "thumbs", move |progress| {
        let project = s2.project.read().unwrap().clone();
        let templates = s2.templates.read().unwrap().clone();
        let wanted: Vec<_> = templates.iter().filter(|(id, _)| req.templates.as_ref().map(|t| t.contains(id)).unwrap_or(true)).collect();
        let n = wanted.len().max(1);
        let mut done = Vec::new();
        for (i, (id, (dir, manifest))) in wanted.iter().enumerate() {
            progress(((i * 100) / n) as u8, &format!("thumbnail {id}"));
            let out = s2.baker.thumbnail(dir, manifest, project.width, project.height).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
            done.push(json!({ "id": id, "thumb": out }));
        }
        s2.reload_templates();
        Ok(json!({ "thumbs": done }))
    });
    Json(json!({ "job": id }))
}

#[derive(Deserialize)]
struct ThumbsReq {
    #[serde(default)]
    templates: Option<Vec<String>>,
}

// ---------- assets ----------

#[derive(Deserialize)]
struct PathQuery {
    path: PathBuf,
}

async fn probe_asset(State(st): S, Query(q): Query<PathQuery>) -> ApiResult {
    let p = jobs::resolve_path(&st, &q.path);
    let pr = blocking(move || kuma_overlay::probe(&p)).await?.map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "kind": pr.kind, "meta": pr.meta })))
}

#[derive(Deserialize)]
struct AddAsset {
    path: PathBuf,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    base_revision: Option<u64>,
    #[serde(default = "default_origin")]
    origin: String,
}

/// Probe and register an asset. The id defaults to the file stem.
async fn add_asset(State(st): S, Json(req): Json<AddAsset>) -> ApiResult {
    let (id, rev, warnings) = blocking(move || jobs::add_asset(&st, &req.path, req.id, req.base_revision, &req.origin)).await??;
    Ok(Json(json!({ "id": id, "revision": rev, "warnings": warnings })))
}

// ---------- jobs ----------

async fn render(State(st): S, Json(req): Json<RenderRequest>) -> Json<Value> {
    let s2 = st.clone();
    let id = jobs::spawn(st, "render", move |progress| jobs::render(&s2, &req, progress));
    Json(json!({ "job": id }))
}

#[derive(Deserialize)]
struct BakeReq {
    #[serde(default)]
    clips: Option<Vec<String>>,
    #[serde(default = "one")]
    supersample: u32,
}
fn one() -> u32 {
    1
}

async fn bake(State(st): S, Json(req): Json<BakeReq>) -> Json<Value> {
    let s2 = st.clone();
    let id = jobs::spawn(st, "bake", move |progress| {
        let project = s2.project.read().unwrap().clone();
        let bakes = jobs::bake_all(&s2, &project, req.clips.as_deref(), kuma_overlay::Format::PngSequence, req.supersample, progress)?;
        let v: Vec<Value> = bakes.iter().map(|(k, b)| json!({ "clip": k, "resource": b.resource, "still": b.is_still, "region": b.region })).collect();
        Ok(json!({ "bakes": v }))
    });
    Json(json!({ "job": id }))
}

/// Synchronous: sampling is what the agent waits on.
async fn sample(State(st): S, Json(req): Json<SampleRequest>) -> ApiResult {
    let samples = blocking(move || jobs::sample_frames(&st, &req, &|_, _| {})).await??;
    Ok(Json(json!({ "samples": samples })))
}

async fn compile_xml(State(st): S) -> Result<Response, ApiError> {
    let project = st.project.read().unwrap().clone();
    let xml = jobs::compile_xml(&st, &project, Default::default(), UnbakedPolicy::Skip, None)?;
    Ok(([(header::CONTENT_TYPE, "application/xml")], xml).into_response())
}

#[derive(Deserialize)]
struct SnapshotQuery {
    t: f64,
    #[serde(default)]
    width: Option<u32>,
}

/// One composited frame as PNG (melt), the pre-engine preview fallback.
async fn snapshot(State(st): S, Query(q): Query<SnapshotQuery>) -> Result<Response, ApiError> {
    let req = SampleRequest { t0: q.t, t1: None, count: 1, rect: None, width: q.width.or(Some(960)), overlays: false };
    let samples = blocking(move || jobs::sample_frames(&st, &req, &|_, _| {})).await??;
    let bytes = std::fs::read(&samples[0].path).map_err(|e| anyhow::anyhow!(e))?;
    Ok(([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")], bytes).into_response())
}

async fn jobs_list(State(st): S) -> Json<Value> {
    Json(json!(st.jobs.list()))
}

async fn job_get(State(st): S, Path(id): Path<u64>) -> Result<Json<Value>, ApiError> {
    st.jobs.get(id).map(|j| Json(json!(j))).ok_or(ApiError::not_found("no such job"))
}

// ---------- websockets ----------

async fn events_ws(State(st): S, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| async move {
        let mut rx = st.events.subscribe();
        let mut socket: WebSocket = socket;
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(ev) => {
                        if socket.send(Message::Text(serde_json::to_string(&ev).unwrap().into())).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    })
}

/// Preview transport: text commands in (`seek <secs>`, `play`, `pause`), binary JPEG
/// frames out, prefixed by an 8-byte little-endian frame index.
async fn preview_ws(State(st): S, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |mut socket| async move {
        let engine = match st.preview.ensure(&st).await {
            Ok(e) => e,
            Err(e) => {
                let _ = socket.send(Message::Text(json!({ "error": format!("{e:#}") }).to_string().into())).await;
                return;
            }
        };
        let _ = socket.send(Message::Text(json!({ "ready": true, "scale": *st.preview.scale.lock().unwrap() }).to_string().into())).await;
        let mut frames = engine.frames.subscribe();
        let fps = st.project.read().unwrap().fps;
        if let Some(e) = reload_into(&st, &engine).await {
            let _ = socket.send(Message::Text(json!({ "error": e }).to_string().into())).await;
        }
        loop {
            tokio::select! {
                f = frames.recv() => match f {
                    Ok(f) => {
                        let mut buf = Vec::with_capacity(8 + f.jpeg.len());
                        buf.extend_from_slice(&f.frame.to_le_bytes());
                        buf.extend_from_slice(&f.jpeg);
                        if socket.send(Message::Binary(Bytes::from(buf))).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        let t = t.to_string();
                        let mut cmd = None;
                        let mut err = None;
                        if let Some(s) = t.strip_prefix("seek ") {
                            cmd = s.trim().parse::<f64>().ok().map(|secs| format!("seek {}", fps.frame(secs)));
                        } else if t == "play" || t == "pause" {
                            cmd = Some(t.clone());
                        } else if t == "reload" {
                            err = reload_into(&st, &engine).await;
                        } else if let Some(s) = t.strip_prefix("scale ") {
                            if let Ok(v) = s.trim().parse::<f64>() {
                                *st.preview.scale.lock().unwrap() = v.clamp(0.25, 1.0);
                                err = reload_into(&st, &engine).await;
                            }
                        }
                        if let Some(c) = cmd {
                            if engine.send(&c).await.is_err() { err = Some("preview engine exited; reconnect"); }
                        }
                        if let Some(e) = err {
                            let _ = socket.send(Message::Text(json!({ "error": e }).to_string().into())).await;
                            if e != "compile failed" { break; }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
        let _ = engine.send("pause").await;
    })
}

/// Recompile and load the preview. A socket is bound to one engine process: if that one
/// died and `reload` started another, the client must reconnect rather than keep
/// listening to a dead channel.
async fn reload_into(st: &Arc<AppState>, engine: &Arc<preview::Engine>) -> Option<&'static str> {
    if preview::reload(st).await.is_err() {
        return Some("compile failed");
    }
    st.preview.get().filter(|e| Arc::ptr_eq(e, engine)).is_none().then_some("preview engine restarted; reconnect")
}

#[derive(Deserialize, Default)]
struct PreviewReload {
    /// Preview resolution as a fraction of the project size (0.25..=1).
    #[serde(default)]
    scale: Option<f64>,
}

async fn preview_reload(State(st): S, body: Option<Json<PreviewReload>>) -> ApiResult {
    if let Some(s) = body.and_then(|b| b.0.scale) {
        *st.preview.scale.lock().unwrap() = s.clamp(0.25, 1.0);
    }
    let path = preview::reload(&st).await?;
    Ok(Json(json!({ "mlt": path, "scale": *st.preview.scale.lock().unwrap() })))
}

// ---------- overlay preview (live DOM) ----------

/// Serve a template instance with kuma's contract injected and the hyperframes runtime
/// preloaded, so the UI can drive it with `HyperframesPlayer.seek(t)`.
async fn overlay_preview(State(st): S, Path(clip): Path<String>) -> Result<Response, ApiError> {
    let project = st.project.read().unwrap().clone();
    let (_, c) = project.find_clip(&clip).ok_or(ApiError::not_found("no such clip"))?;
    let ClipSource::Overlay { overlay, vars } = &c.src else {
        return Err(ApiError::bad_request("not an overlay clip"));
    };
    let templates = st.templates.read().unwrap();
    let (dir, manifest) = templates.get(overlay).ok_or(ApiError::not_found("unknown template"))?;
    let resolved = manifest.resolve_vars(vars);
    let html = std::fs::read_to_string(dir.join(&manifest.entry)).map_err(|e| anyhow::anyhow!(e))?;
    // Clip ids are validated to be URL-safe.
    let head_extra = format!("<base href=\"/overlay-preview/{}/\">\n<script src=\"/vendor/hyperframe.runtime.iife.js\"></script>\n", c.id);
    let out = kuma_overlay::Contract {
        vars: &resolved,
        fps: project.fps,
        width: project.width,
        height: project.height,
        duration: c.duration() * c.speed.abs(),
        speed: c.speed,
        bake: false,
        clip: Some(&c.id),
        head_extra: &head_extra,
    }
    .apply(&html);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], out).into_response())
}

/// Files referenced by a previewed overlay, resolved against its template dir.
async fn overlay_file(State(st): S, Path((clip, rest)): Path<(String, String)>) -> Result<Response, ApiError> {
    let project = st.project.read().unwrap().clone();
    let (_, c) = project.find_clip(&clip).ok_or(ApiError::not_found("no such clip"))?;
    let ClipSource::Overlay { overlay, .. } = &c.src else {
        return Err(ApiError::bad_request("not an overlay clip"));
    };
    serve_template_file(&st, overlay, &rest)
}

async fn overlay_static(State(st): S, Path((id, rest)): Path<(String, String)>) -> Result<Response, ApiError> {
    serve_template_file(&st, &id, &rest)
}

/// A file inside a template folder. `_vendor/...` resolves exactly as the bake stages it.
fn serve_template_file(st: &AppState, template: &str, rest: &str) -> Result<Response, ApiError> {
    let templates = st.templates.read().unwrap();
    let (dir, _) = templates.get(template).ok_or(ApiError::not_found("unknown template"))?;
    let mut path = safe_join(dir, rest).ok_or(ApiError::bad_request("bad path"))?;
    if !path.is_file() {
        if let Some((vendor, v)) = rest.strip_prefix("_vendor/").and_then(|v| st.baker.vendor_for(dir).map(|d| (d, v))) {
            path = safe_join(&vendor, v).ok_or(ApiError::bad_request("bad path"))?;
        }
    }
    file_response(&path)
}

async fn vendor(State(st): S, Path(rest): Path<String>) -> Result<Response, ApiError> {
    let nm = st.config.repo_root.join("node_modules");
    let bases = [nm.join("hyperframes/dist"), nm.join("gsap/dist"), st.baker.vendor_dir.clone()];
    bases
        .iter()
        .filter_map(|b| safe_join(b, &rest))
        .find(|c| c.is_file())
        .map(|c| file_response(&c))
        .unwrap_or_else(|| Err(ApiError::not_found("not found")))
}

/// Serve a file under the project root (renders, samples, bakes) or the overlay library
/// (thumbnails). Anything else, including the rest of the repo, is refused.
async fn file(State(st): S, Query(q): Query<PathQuery>) -> Result<Response, ApiError> {
    let canon = jobs::resolve_path(&st, &q.path).canonicalize().map_err(|_| ApiError::not_found("not found"))?;
    let allowed = [st.config.project_root(), st.config.repo_root.join("overlays")];
    if !allowed.iter().filter_map(|d| d.canonicalize().ok()).any(|d| canon.starts_with(d)) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "outside the project"));
    }
    file_response(&canon)
}

fn file_response(path: &std::path::Path) -> Result<Response, ApiError> {
    let bytes = std::fs::read(path).map_err(|_| ApiError::not_found("not found"))?;
    let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    };
    let mut resp = bytes.into_response();
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    Ok(resp)
}
