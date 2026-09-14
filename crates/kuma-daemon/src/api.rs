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
use kuma_core::{validate, BinSource, Project};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;

use crate::jobs::{self, RenderRequest, SampleRequest};
use crate::preview::{self, EngineEvent};
use crate::state::{lock, read, AppState};

type S = State<Arc<AppState>>;

pub fn router(state: Arc<AppState>) -> Router {
    let mut app = Router::new()
        .route("/api/health", get(health))
        .route("/api/project", get(get_project))
        .route("/api/project/patch", post(patch_project))
        .route("/api/project/undo", post(undo))
        .route("/api/project/redo", post(redo))
        .route("/api/project/validate", get(validate_project))
        .route("/api/schema", get(schema))
        .route("/api/templates", get(templates).post(reload_templates))
        .route("/api/templates/thumbs", post(thumbs))
        .route("/api/presets/effects", get(effect_presets))
        .route("/api/media/probe", get(probe_media))
        .route("/api/bin", post(add_media))
        .route("/api/bin/{id}/thumb.png", get(bin_thumb))
        .route("/api/presets", get(presets))
        .route("/api/render", post(render))
        .route("/api/bake", post(bake))
        .route("/api/sample", post(sample))
        .route("/api/snapshot", get(snapshot))
        .route("/api/jobs/{id}", get(job_get))
        .route("/api/events", get(events_ws))
        .route("/api/preview/ws", get(preview_ws))
        .route("/api/preview/reload", post(preview_reload))
        .route("/overlay-preview/{clip}", get(overlay_preview))
        .route("/overlay-preview/{clip}/{*rest}", get(overlay_file))
        .route("/overlays/{id}/{*rest}", get(overlay_static))
        .route("/vendor/{*rest}", get(vendor));
    let ui = state.config.repo_root.join("ui/dist");
    if ui.is_dir() {
        app = app.fallback_service(ServeDir::new(ui).append_index_html_on_directories(true));
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

/// Cheap and lock-free: tool probes belong to `kuma doctor`, not to a call every client
/// makes at startup (a slow probe used to make `kuma mcp` think no daemon was running).
async fn health(State(st): S) -> Json<Value> {
    let revision = read(&st.project).revision;
    Json(json!({
        "ok": true,
        "project": st.config.project_file,
        "revision": revision,
        "hyperframes": st.baker.version,
        "browser": st.baker.browser,
        "template_dirs": st.config.all_template_dirs(),
        "template_errors": read(&st.templates).errors,
        "preview_engine": preview::PreviewHandle::engine_path(&st).is_file(),
        "preview_running": st.preview.get().is_some(),
        "preview_error": lock(&st.preview.last_error).clone(),
    }))
}

async fn get_project(State(st): S) -> Json<Project> {
    Json(read(&st.project).clone())
}

#[derive(Deserialize)]
struct PatchProject {
    patch: json_patch::Patch,
    #[serde(default)]
    base_revision: Option<u64>,
    #[serde(default = "default_origin")]
    origin: String,
}
fn default_origin() -> String {
    "ui".into()
}

// Mutations validate, serialize and write the file under the edit lock, so they run on
// the blocking pool rather than an async worker.
async fn patch_project(State(st): S, Json(req): Json<PatchProject>) -> ApiResult {
    let (rev, warnings) = blocking(move || st.apply_patch(&req.patch.0, req.base_revision, &req.origin)).await??;
    Ok(Json(json!({ "revision": rev, "warnings": warnings })))
}

async fn undo(State(st): S) -> ApiResult {
    Ok(Json(json!({ "revision": blocking(move || st.undo()).await?? })))
}
async fn redo(State(st): S) -> ApiResult {
    Ok(Json(json!({ "revision": blocking(move || st.redo()).await?? })))
}

async fn validate_project(State(st): S) -> Json<Value> {
    let p = read(&st.project).clone();
    let issues = validate(&p, &st.validation_context());
    Json(json!({ "issues": issues, "duration": p.duration() }))
}

async fn schema() -> Json<Value> {
    Json(json!({ "project": kuma_core::schema::project_schema(), "template": kuma_core::schema::template_schema() }))
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

/// Effect presets: `presets/*.json` from every library, then the project (wins on id).
async fn effect_presets(State(st): S) -> Json<Value> {
    let mut out: std::collections::BTreeMap<String, Value> = Default::default();
    for dir in st.config.all_preset_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for p in rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
            if let Some(v) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
                if let Some(id) = v.get("id").and_then(Value::as_str) {
                    out.insert(id.to_string(), v);
                }
            }
        }
    }
    Json(json!(out.into_values().collect::<Vec<_>>()))
}

#[derive(Deserialize)]
struct ThumbsReq {
    #[serde(default)]
    templates: Option<Vec<String>>,
}

/// Generate `thumb.png` for every template (or the given ids) as a background job.
async fn thumbs(State(st): S, Json(req): Json<ThumbsReq>) -> Json<Value> {
    let s2 = st.clone();
    let id = jobs::spawn(st, "thumbs", move |progress| {
        let templates = read(&s2.templates).loaded.clone();
        let wanted: Vec<_> = templates.iter().filter(|(id, _)| req.templates.as_ref().map(|t| t.contains(id)).unwrap_or(true)).collect();
        // Every overlay bin entry too, rendered with its own vars: two entries of one
        // template look different in the browser because they *are* different.
        let entries: Vec<(String, String, serde_json::Map<String, Value>)> = read(&s2.project)
            .bin
            .iter()
            .filter_map(|(id, item)| match &item.source {
                BinSource::Overlay { template, vars } => Some((id.clone(), template.clone(), vars.clone())),
                _ => None,
            })
            .collect();
        let n = (wanted.len() + entries.len()).max(1);
        let mut done = Vec::new();
        let empty = serde_json::Map::new();
        for (i, (id, (dir, manifest))) in wanted.iter().enumerate() {
            progress(((i * 100) / n) as u8, &format!("thumbnail {id}"));
            let out = dir.join("thumb.png");
            s2.baker.thumbnail(dir, manifest, &empty, &out).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
            done.push(json!({ "template": id, "thumb": out }));
        }
        for (i, (id, template, vars)) in entries.iter().enumerate() {
            progress((((wanted.len() + i) * 100) / n) as u8, &format!("thumbnail {id}"));
            let Some((dir, manifest)) = templates.get(template) else { continue };
            let out = bin_thumb_path(&s2, id);
            s2.baker.thumbnail(dir, manifest, vars, &out).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
            done.push(json!({ "bin": id, "thumb": out }));
        }
        s2.reload_templates();
        Ok(json!({ "thumbs": done }))
    });
    Json(json!({ "job": id }))
}

fn bin_thumb_path(st: &AppState, bin_id: &str) -> PathBuf {
    st.config.cache_dir().join("thumbs").join(format!("{bin_id}.png"))
}

/// A bin entry's thumbnail, generated by the `thumbs` job. 404 until then.
async fn bin_thumb(State(st): S, Path(id): Path<String>) -> Result<Response, ApiError> {
    if !kuma_core::is_valid_id(&id) {
        return Err(ApiError::bad_request("bad bin id"));
    }
    file_response(&bin_thumb_path(&st, &id))
}

// ---------- assets ----------

#[derive(Deserialize)]
struct PathQuery {
    path: PathBuf,
}

async fn probe_media(State(st): S, Query(q): Query<PathQuery>) -> ApiResult {
    let p = jobs::resolve_path(&st, &q.path);
    let pr = blocking(move || kuma_overlay::probe(&p)).await?.map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "kind": pr.kind, "meta": pr.meta })))
}

#[derive(Deserialize)]
struct AddMedia {
    path: PathBuf,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    base_revision: Option<u64>,
    #[serde(default = "default_origin")]
    origin: String,
}

/// Probe a media file and add it to the bin. The id defaults to the file stem.
async fn add_media(State(st): S, Json(req): Json<AddMedia>) -> ApiResult {
    let (id, rev, warnings) = blocking(move || jobs::add_media(&st, &req.path, req.id, req.base_revision, &req.origin))
        .await?
        // Keep the 409/422 contract of every other edit; anything else is the caller's input.
        .map_err(|e| match e.downcast::<EditError>() {
            Ok(edit) => ApiError::from(edit),
            Err(other) => ApiError::bad_request(format!("{other:#}")),
        })?;
    Ok(Json(json!({ "id": id, "revision": rev, "warnings": warnings })))
}

// ---------- jobs ----------

async fn render(State(st): S, Json(req): Json<RenderRequest>) -> Json<Value> {
    let s2 = st.clone();
    Json(json!({ "job": jobs::spawn(st, "render", move |progress| jobs::render(&s2, &req, progress)) }))
}

#[derive(Deserialize)]
struct BakeReq {
    #[serde(default)]
    clips: Option<Vec<String>>,
}

async fn bake(State(st): S, Json(req): Json<BakeReq>) -> Json<Value> {
    let s2 = st.clone();
    let id = jobs::spawn(st, "bake", move |progress| {
        let project = read(&s2.project).clone();
        let bakes = jobs::bake_all(&s2, &project, req.clips.as_deref(), progress)?;
        Ok(json!({ "bakes": bakes.iter().map(|(k, b)| json!({ "clip": k, "resource": b.resource, "still": b.is_still })).collect::<Vec<_>>() }))
    });
    Json(json!({ "job": id }))
}

/// Synchronous: sampling is what the agent waits on.
async fn sample(State(st): S, Json(req): Json<SampleRequest>) -> ApiResult {
    let samples = blocking(move || jobs::sample_frames(&st, &req, &|_, _| {})).await??;
    Ok(Json(json!({ "samples": samples })))
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

async fn job_get(State(st): S, Path(id): Path<u64>) -> ApiResult {
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

/// Preview transport: text commands in (`seek <secs>`, `play`, `pause`, `reload`,
/// `scale <f>`), JSON status lines and binary JPEG frames out (8-byte little-endian frame
/// index, then the JPEG). The socket closes when its engine exits; the client reconnects.
async fn preview_ws(State(st): S, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |mut socket| async move {
        async fn say(socket: &mut WebSocket, v: Value) -> bool {
            socket.send(Message::Text(v.to_string().into())).await.is_ok()
        }
        let engine = match st.preview.ensure(&st).await {
            Ok(e) => e,
            Err(e) => {
                say(&mut socket, json!({ "unavailable": format!("{e:#}") })).await;
                return;
            }
        };
        let mut events = engine.events.subscribe();
        say(&mut socket, json!({ "ready": true, "scale": *lock(&st.preview.scale) })).await;
        // A compile error is reported and the socket stays; a transport error means the
        // engine is gone and the socket goes with it.
        macro_rules! reload_or_break {
            () => {
                if let Err(e) = preview::reload(&st).await {
                    let compile_error = e.downcast_ref::<kuma_mlt::CompileError>().is_some();
                    say(&mut socket, json!({ "error": format!("{e:#}") })).await;
                    if !compile_error { break; }
                }
            };
        }
        loop {
            reload_or_break!();
            break;
        }
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Ok(EngineEvent::Frame(f)) => {
                        let mut buf = Vec::with_capacity(8 + f.jpeg.len());
                        buf.extend_from_slice(&f.frame.to_le_bytes());
                        buf.extend_from_slice(&f.jpeg);
                        if socket.send(Message::Binary(Bytes::from(buf))).await.is_err() { break; }
                    }
                    Ok(EngineEvent::End) => { if !say(&mut socket, json!({ "end": true })).await { break; } }
                    Ok(EngineEvent::Exited) | Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                },
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        let t = t.to_string();
                        if let Some(s) = t.strip_prefix("seek ") {
                            // Read the fps per seek: the project may have changed it meanwhile.
                            let fps = read(&st.project).fps;
                            if let Ok(secs) = s.trim().parse::<f64>() {
                                if engine.send(&format!("seek {}", fps.frame(secs))).await.is_err() { break; }
                            }
                        } else if t == "play" || t == "pause" {
                            if engine.send(&t).await.is_err() { break; }
                        } else if t == "reload" {
                            reload_or_break!();
                        } else if let Some(s) = t.strip_prefix("scale ") {
                            if let Ok(v) = s.trim().parse::<f64>() {
                                *lock(&st.preview.scale) = v.clamp(0.25, 1.0);
                                reload_or_break!();
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
        // The engine is shared by every open window: only the last one leaving pauses it.
        drop(events);
        if engine.events.receiver_count() == 0 {
            let _ = engine.send("pause").await;
        }
    })
}

#[derive(Deserialize, Default)]
struct PreviewReload {
    /// Preview resolution as a fraction of the project size (0.25..=1).
    #[serde(default)]
    scale: Option<f64>,
}

async fn preview_reload(State(st): S, body: Option<Json<PreviewReload>>) -> ApiResult {
    if let Some(s) = body.and_then(|b| b.0.scale) {
        *lock(&st.preview.scale) = s.clamp(0.25, 1.0);
    }
    let path = preview::reload(&st).await?;
    Ok(Json(json!({ "mlt": path, "scale": *lock(&st.preview.scale) })))
}

// ---------- overlay preview (live DOM) ----------

/// Serve a template instance with kuma's contract injected and the hyperframes runtime
/// preloaded, so the UI can drive it with `seek(t)`.
async fn overlay_preview(State(st): S, Path(clip): Path<String>) -> Result<Response, ApiError> {
    let project = read(&st.project).clone();
    let (_, c) = project.find_clip(&clip).ok_or(ApiError::not_found("no such clip"))?;
    // The bin entry decides the template and its vars; the clip decides how long it runs.
    let Some(BinSource::Overlay { template, vars }) = project.item(c).map(|i| &i.source) else {
        return Err(ApiError::bad_request("not an overlay clip"));
    };
    let templates = read(&st.templates);
    let (dir, manifest) = templates.loaded.get(template).ok_or(ApiError::not_found("unknown template"))?;
    let html = std::fs::read_to_string(dir.join("index.html")).map_err(|e| anyhow::anyhow!(e))?;
    // Clip ids are validated to be URL-safe.
    let head_extra = format!("<base href=\"/overlay-preview/{}/\">\n<script src=\"/vendor/hyperframe.runtime.iife.js\"></script>\n", c.id);
    let out = kuma_overlay::Contract {
        vars: &manifest.resolve_vars(vars),
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
    let project = read(&st.project).clone();
    let (_, c) = project.find_clip(&clip).ok_or(ApiError::not_found("no such clip"))?;
    let Some(BinSource::Overlay { template, .. }) = project.item(c).map(|i| &i.source) else {
        return Err(ApiError::bad_request("not an overlay clip"));
    };
    serve_template_file(&st, template, &rest)
}

async fn overlay_static(State(st): S, Path((id, rest)): Path<(String, String)>) -> Result<Response, ApiError> {
    serve_template_file(&st, &id, &rest)
}

/// A file inside a template folder. `_vendor/...` resolves exactly as the bake stages it.
fn serve_template_file(st: &AppState, template: &str, rest: &str) -> Result<Response, ApiError> {
    let templates = read(&st.templates);
    let (dir, _) = templates.loaded.get(template).ok_or(ApiError::not_found("unknown template"))?;
    let mut path = safe_join(dir, rest).ok_or(ApiError::bad_request("bad path"))?;
    if !path.is_file() {
        if let Some((vendor, v)) = rest.strip_prefix("_vendor/").and_then(|v| st.baker.vendor_for(dir).map(|d| (d, v))) {
            path = safe_join(&vendor, v).ok_or(ApiError::bad_request("bad path"))?;
        }
    }
    file_response(&path)
}

/// The hyperframes runtime (from the repo's `node_modules`) and the shared `_vendor/`.
async fn vendor(State(st): S, Path(rest): Path<String>) -> Result<Response, ApiError> {
    let bases = [st.config.repo_root.join("node_modules/hyperframes/dist"), st.baker.vendor_dir.clone()];
    bases
        .iter()
        .filter_map(|b| safe_join(b, &rest))
        .find(|c| c.is_file())
        .map(|c| file_response(&c))
        .unwrap_or_else(|| Err(ApiError::not_found("not found")))
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
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    };
    let mut resp = bytes.into_response();
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    Ok(resp)
}
