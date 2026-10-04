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
use kuma_core::{BinSource, Project};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::jobs::{self, MeasureRequest, MediaMeasureRequest, RenderRequest, SampleRequest, TranscribeRequest};
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
        .route("/api/sequence", get(get_sequence).post(open_sequence))
        .route("/api/schema", get(schema))
        .route("/api/blueprints", get(blueprints).post(reload_blueprints))
        .route("/api/blueprints/thumbs", post(thumbs))
        .route("/api/blueprints/{id}/copy", post(copy_blueprint))
        .route("/api/blueprints/{id}/lint", get(lint_blueprint))
        .route("/api/overlays", get(overlays))
        .route("/api/overlays/new", post(new_overlay))
        .route("/api/overlays/lint", get(lint_overlay))
        .route("/api/overlays/import", post(import_overlay))
        .route("/api/overlays/unique", post(make_unique))
        .route("/api/overlays/save-blueprint", post(save_blueprint))
        .route("/api/fonts", get(fonts).post(add_font))
        .route("/api/fonts.css", get(fonts_css))
        .route("/fonts/{slug}/{file}", get(font_file))
        .route("/api/presets/effects", get(effect_presets))
        .route("/api/media/probe", get(probe_media))
        .route("/api/bin", post(add_media))
        .route("/api/transcribe", post(transcribe))
        .route("/api/bin/{id}/{file}", get(bin_asset))
        .route("/api/presets", get(presets))
        .route("/api/render", post(render))
        .route("/api/render/target", get(render_target))
        .route("/api/bake", post(bake))
        .route("/api/sample", post(sample))
        .route("/api/sample/job", post(sample_job))
        .route("/api/audio/measure", post(measure_audio))
        .route("/api/media/measure", post(measure_media))
        .route("/api/snapshot", get(snapshot))
        .route("/api/cache", get(cache_usage))
        .route("/api/cache/prune", post(cache_prune))
        .route("/api/jobs", get(job_list))
        .route("/api/jobs/clear", post(job_clear))
        .route("/api/jobs/{id}", get(job_get))
        .route("/api/jobs/{id}/cancel", post(job_cancel))
        .route("/api/events", get(events_ws))
        .route("/api/preview/ws", get(preview_ws))
        .route("/api/preview/reload", post(preview_reload))
        .route("/overlay-preview/{clip}", get(overlay_preview))
        .route("/overlay-preview/{clip}/{*rest}", get(overlay_file))
        .route("/overlay-entry/{id}", get(overlay_entry))
        .route("/overlay-entry/{id}/{*rest}", get(overlay_entry_file))
        .route("/blueprints/{id}/{*rest}", get(blueprint_static))
        .route("/vendor/{*rest}", get(vendor));
    let ui = state.config.repo_root.join("ui/dist");
    if ui.is_dir() {
        app = app.fallback_service(ServeDir::new(ui).append_index_html_on_directories(true));
    }
    // Nothing this daemon serves may be reused without asking first.
    //
    // Without a `Cache-Control` there is no `ETag` either, so a browser falls back to
    // *heuristic* freshness: it invents a lifetime from `Last-Modified` and serves the
    // response for as long as it likes without revalidating. For `ui/dist` that means a
    // rebuilt editor has no effect on a window whose profile cached the old bundle — it
    // keeps running it and fails on the shape of a newer project. The same applies to an
    // overlay being authored, to a regenerated thumbnail that keeps its URL, and to API
    // JSON, which is live state and was never safe to cache.
    //
    // `no-cache` still allows storing it; it only requires revalidation, which on
    // loopback costs a 304. `if_not_present` so the handlers that say `no-store` (the
    // overlay preview, snapshots) keep the stronger rule they chose.
    app.layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))).with_state(state)
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

/// An entry that is not there — or whose overlay folder is not — is gone; one that is
/// there and is not an overlay is the wrong thing to have asked for.
impl From<jobs::EntryError> for ApiError {
    fn from(e: jobs::EntryError) -> Self {
        match e {
            jobs::EntryError::NotOverlay(_) => Self::bad_request(e),
            _ => Self::not_found(e),
        }
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
        "blueprint_dirs": st.config.blueprint_dirs(),
        "blueprints_loaded": read(&st.blueprints).loaded.len(),
        "blueprint_errors": read(&st.blueprints).errors,
        // Not loaded by the daemon: a design system is a folder copied into the project
        // for an agent to read (`kuma design-system <id>`). Reported so `doctor` and an
        // agent can see whether this project has one.
        "design_system": st.config.project_design_system(),
        "preview_engine": preview::PreviewHandle::engine_path(&st).is_file(),
        "preview_running": st.preview.get().is_some(),
        "sequence": st.current_sequence(),
        "preview_error": lock(&st.preview.last_error).clone(),
        "whisper": st.transcriber.available().map(|m| json!(m)).unwrap_or_else(|e| json!({ "error": e })),
    }))
}

async fn get_project(State(st): S) -> Json<Project> {
    Json(read(&st.project).clone())
}

#[derive(Deserialize)]
struct PatchProject {
    #[serde(default)]
    patch: Option<json_patch::Patch>,
    /// Or a file in the project holding the patch, a JSON array of operations. A script
    /// that builds a video writes its patch there and the daemon reads it, so a patch of any
    /// size is applied exactly as written instead of being copied through a client in chunks.
    #[serde(default)]
    patch_path: Option<String>,
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
    let out = blocking(move || -> Result<Value, ApiError> {
        let patch = match (req.patch, req.patch_path) {
            (Some(patch), None) => patch,
            (None, Some(path)) => read_patch(&st, &path).map_err(bad_request_err)?,
            _ => return Err(ApiError::bad_request("pass the operations as `patch`, or a file in the project holding them as `patch_path`: one of the two")),
        };
        let (rev, warnings) = st.apply_patch(&patch.0, req.base_revision, &req.origin)?;
        Ok(json!({ "revision": rev, "warnings": warnings, "ops": patch.0.len() }))
    })
    .await??;
    Ok(Json(out))
}

/// The JSON Patch in the file at `path`, which must be inside the project: relative to it,
/// or absolute and under it.
fn read_patch(st: &AppState, path: &str) -> anyhow::Result<json_patch::Patch> {
    use anyhow::Context;
    let file = jobs::resolve_path(st, std::path::Path::new(path)).canonicalize().with_context(|| format!("cannot read `{path}`"))?;
    anyhow::ensure!(file.starts_with(st.config.project_root().canonicalize()?), "`{path}` is not inside the project");
    let text = std::fs::read_to_string(&file).with_context(|| format!("cannot read `{path}`"))?;
    serde_json::from_str(&text).with_context(|| format!("`{path}` is not a JSON Patch (an array of operations)"))
}

async fn undo(State(st): S) -> ApiResult {
    Ok(Json(json!({ "revision": blocking(move || st.undo()).await?? })))
}
async fn redo(State(st): S) -> ApiResult {
    Ok(Json(json!({ "revision": blocking(move || st.redo()).await?? })))
}

/// The document's issues, and what its overlay entries will not be drawn in (fonts that are
/// not installed, characters no font of theirs sets): warnings no edit is refused for, but
/// that a video should not be rendered with unread.
async fn validate_project(State(st): S) -> ApiResult {
    let out = blocking(move || {
        let p = read(&st.project).clone();
        json!({ "issues": st.validate(&p), "duration": p.duration() })
    })
    .await?;
    Ok(Json(out))
}

/// Which sequence is open for editing. Session state: it is not in the document and does
/// not survive a restart, which is exactly why it needs an endpoint of its own — what
/// sequences exist, and how long each is, the client reads off the project it already has.
async fn get_sequence(State(st): S) -> Json<Value> {
    Json(json!({ "current": st.current_sequence() }))
}

#[derive(Deserialize)]
struct OpenSequence {
    /// Sequence to open. `null` goes back to following the master.
    #[serde(default)]
    sequence: Option<String>,
}

async fn open_sequence(State(st): S, Json(req): Json<OpenSequence>) -> ApiResult {
    let current = st.open_sequence(req.sequence).map_err(ApiError::bad_request)?;
    // The engine holds one compiled timeline, so opening a sequence is a reload.
    let reloaded = preview::reload(&st).await.is_ok();
    Ok(Json(json!({ "current": current, "preview_reloaded": reloaded })))
}

async fn schema() -> Json<Value> {
    Json(json!({ "project": kuma_core::schema::project_schema(), "overlay": kuma_core::schema::overlay_schema() }))
}

async fn blueprints(State(st): S) -> Json<Value> {
    Json(json!(st.blueprint_infos()))
}

async fn reload_blueprints(State(st): S) -> Json<Value> {
    st.reload_blueprints();
    Json(json!(st.blueprint_infos()))
}

fn bad_request_err(e: anyhow::Error) -> ApiError {
    ApiError::bad_request(format!("{e:#}"))
}

/// A bad request, except an edit refusal, which keeps its 409/422.
fn edit_or_bad_request(e: anyhow::Error) -> ApiError {
    match e.downcast::<EditError>() {
        Ok(edit) => ApiError::from(edit),
        Err(other) => bad_request_err(other),
    }
}

#[derive(Deserialize, Default)]
struct CopyBlueprint {
    #[serde(default)]
    folder: Option<String>,
}

/// Copy a blueprint into the project, without an edit: the editor adds the entry in the
/// same patch as the clip it drops.
async fn copy_blueprint(State(st): S, Path(id): Path<String>, body: Option<Json<CopyBlueprint>>) -> ApiResult {
    let folder = body.and_then(|b| b.0.folder);
    let overlay = blocking(move || crate::overlays::copy_blueprint(&st, &id, folder.as_deref())).await?.map_err(bad_request_err)?;
    Ok(Json(json!({ "overlay": overlay })))
}

async fn lint_blueprint(State(st): S, Path(id): Path<String>) -> ApiResult {
    let report = blocking(move || crate::overlays::lint_blueprint(&st, &id)).await?.map_err(bad_request_err)?;
    Ok(Json(json!(report)))
}

/// The manifest of every overlay folder the bin uses, or why it does not load.
async fn overlays(State(st): S) -> ApiResult {
    let folders: Vec<String> = read(&st.project).overlay_folders().into_iter().map(str::to_string).collect();
    let out = blocking(move || {
        folders
            .into_iter()
            .map(|rel| {
                let v = match crate::overlays::resolve(&st, &rel) {
                    Ok(r) => json!({ "manifest": r.manifest }),
                    Err(e) => json!({ "error": format!("{e:#}") }),
                };
                (rel, v)
            })
            .collect::<serde_json::Map<String, Value>>()
    })
    .await?;
    Ok(Json(Value::Object(out)))
}

#[derive(Deserialize)]
struct NewOverlay {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

/// Scaffold `overlays/<id>/` and lint it. Not an edit: the folder reaches the bin when
/// imported. A failed lint is reported beside the folder, which exists either way.
async fn new_overlay(State(st): S, Json(req): Json<NewOverlay>) -> ApiResult {
    let dir = kuma_overlay::new_overlay(&st.config.overlays_dir(), &req.id, &req.name, &req.description).map_err(ApiError::bad_request)?;
    let overlay = format!("overlays/{}", req.id);
    let lint = blocking({ let (st, dir) = (st.clone(), dir.clone()); move || crate::overlays::lint(&st, &dir) }).await?;
    Ok(Json(match lint {
        Ok(report) => json!({ "overlay": overlay, "dir": dir, "lint": report }),
        Err(e) => json!({ "overlay": overlay, "dir": dir, "lint": Value::Null, "lint_error": format!("{e:#}") }),
    }))
}

#[derive(Deserialize)]
struct OverlayPath {
    path: String,
}

async fn lint_overlay(State(st): S, Query(q): Query<OverlayPath>) -> ApiResult {
    let report = blocking(move || crate::overlays::lint_overlay(&st, &q.path)).await?.map_err(bad_request_err)?;
    Ok(Json(json!(report)))
}

#[derive(Deserialize)]
struct ImportOverlay {
    path: String,
    #[serde(flatten)]
    how: crate::overlays::Import,
    #[serde(default)]
    base_revision: Option<u64>,
    #[serde(default = "default_origin")]
    origin: String,
}

async fn import_overlay(State(st): S, Json(req): Json<ImportOverlay>) -> ApiResult {
    let (id, rev, warnings) =
        blocking(move || crate::overlays::import_overlay(&st, &req.path, req.how, req.base_revision, &req.origin)).await?.map_err(edit_or_bad_request)?;
    Ok(Json(json!({ "id": id, "revision": rev, "warnings": warnings })))
}

#[derive(Deserialize)]
struct MakeUnique {
    bin: String,
    #[serde(default = "default_origin")]
    origin: String,
}

async fn make_unique(State(st): S, Json(req): Json<MakeUnique>) -> ApiResult {
    let (overlay, rev) = blocking(move || crate::overlays::make_unique(&st, &req.bin, &req.origin)).await?.map_err(edit_or_bad_request)?;
    Ok(Json(json!({ "overlay": overlay, "revision": rev })))
}

#[derive(Deserialize)]
struct SaveBlueprint {
    bin: String,
    id: String,
    #[serde(default)]
    force: bool,
}

async fn save_blueprint(State(st): S, Json(req): Json<SaveBlueprint>) -> ApiResult {
    let dir = blocking(move || crate::overlays::save_blueprint(&st, &req.bin, &req.id, req.force)).await?.map_err(bad_request_err)?;
    Ok(Json(json!({ "dir": dir })))
}

// ---------- fonts ----------

/// Every family a `font` var can name, with where it comes from and the faces it has.
async fn fonts(State(st): S) -> ApiResult {
    let families = blocking(move || st.baker.fonts.families()).await?;
    Ok(Json(json!(families.into_values().collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct AddFont {
    /// A `.ttf`/`.otf` file, or a `.ttc`/`.otc` collection, relative to the project or absolute.
    path: PathBuf,
    /// Into the user library for every project, rather than this project's `fonts/`.
    #[serde(default)]
    shared: bool,
}

/// Copy a font file in. Not an edit — a font is a file beside the project, the way an
/// overlay folder is — so nothing is undone and the bin is untouched.
async fn add_font(State(st): S, Json(req): Json<AddFont>) -> ApiResult {
    let out = blocking(move || crate::fonts::add_font(&st, &req.path, req.shared)).await?.map_err(bad_request_err)?;
    Ok(Json(out))
}

/// Where fonts are served, `<route><slug>/<file>`: to the editor's font picker and to
/// every overlay the preview renders.
const FONT_ROUTE: &str = "/fonts/";

/// `@font-face` for every family, for the editor's own page: the picker shows each name
/// in its face.
async fn fonts_css(State(st): S) -> Result<Response, ApiError> {
    let css: String = blocking(move || st.baker.fonts.families().values().map(|f| f.css(FONT_ROUTE)).collect()).await?;
    Ok(([(header::CONTENT_TYPE, "text/css")], css).into_response())
}

async fn font_file(State(st): S, Path((slug, file)): Path<(String, String)>) -> Result<Response, ApiError> {
    let path = blocking(move || st.baker.fonts.file(&slug, &file)).await?;
    file_response(&path.ok_or(ApiError::not_found("no such font"))?)
}

/// Export presets, each with why this machine cannot render with it, if it cannot. Trying
/// a hardware one takes a second, once per daemon, so it happens off the async threads.
async fn presets() -> ApiResult {
    Ok(Json(json!(blocking(kuma_mlt::presets::offered).await?)))
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
    /// Blueprints to render. With neither list, everything is rendered.
    #[serde(default)]
    blueprints: Option<Vec<String>>,
    /// Overlay bin entries to render.
    #[serde(default)]
    bins: Option<Vec<String>>,
}

/// Previews (a still and an animated WebP) for blueprints and overlay bin entries, as a
/// background job: the ones named, or all of them.
async fn thumbs(State(st): S, Json(req): Json<ThumbsReq>) -> Json<Value> {
    let s2 = st.clone();
    let id = jobs::spawn(st, "thumbs", move |progress| {
        let _cache = read(&s2.cache_users);
        let all = req.blueprints.is_none() && req.bins.is_none();
        let wants = |list: &Option<Vec<String>>, id: &String| all || list.as_ref().is_some_and(|l| l.contains(id));
        let blueprints = read(&s2.blueprints).loaded.clone();
        let wanted: Vec<_> = blueprints.iter().filter(|(id, _)| wants(&req.blueprints, id)).collect();
        // Every overlay bin entry too, rendered with its own vars: two entries of one
        // folder look different in the browser because they *are* different.
        let entries: Vec<(String, String, serde_json::Map<String, Value>)> = read(&s2.project)
            .bin
            .iter()
            .filter(|(id, _)| wants(&req.bins, id))
            .filter_map(|(id, item)| match &item.source {
                BinSource::Overlay { overlay, vars } => Some((id.clone(), overlay.clone(), vars.clone())),
                _ => None,
            })
            .collect();
        let n = (wanted.len() + entries.len()).max(1);
        let mut done = Vec::new();
        let empty = serde_json::Map::new();
        // A still and a moving preview of the same thing: the still is the poster the
        // browser falls back to, the WebP is what an overlay actually is.
        for (i, (id, (dir, manifest))) in wanted.iter().enumerate() {
            progress(((i * 100) / n) as u8, &format!("thumbnail {id}"));
            let out = dir.join("thumb.png");
            s2.baker.thumbnail(dir, manifest, &empty, &out).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
            let motion = dir.join("preview.webp");
            let preview = s2.baker.preview(dir, manifest, &empty, &motion);
            done.push(json!({ "blueprint": id, "thumb": out, "preview": preview.as_ref().ok().map(|_| motion), "preview_error": preview.err().map(|e| e.to_string()) }));
        }
        for (i, (id, overlay, vars)) in entries.iter().enumerate() {
            progress((((wanted.len() + i) * 100) / n) as u8, &format!("thumbnail {id}"));
            // An entry whose folder is broken has nothing to show; validation says why.
            let Ok(r) = crate::overlays::resolve(&s2, overlay) else { continue };
            let out = bin_asset_path(&s2, id, "png");
            s2.baker.thumbnail(&r.dir, &r.manifest, vars, &out).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
            let motion = bin_asset_path(&s2, id, "webp");
            // A preview is a nicety: an overlay whose still baked and whose animation does
            // not encode (no libwebp in this ffmpeg) must not fail the whole job.
            let preview = s2.baker.preview(&r.dir, &r.manifest, vars, &motion);
            done.push(json!({ "bin": id, "thumb": out, "preview": preview.as_ref().ok().map(|_| motion), "preview_error": preview.err().map(|e| e.to_string()) }));
        }
        s2.reload_blueprints();
        Ok(json!({ "thumbs": done }))
    });
    Json(json!({ "job": id }))
}

/// Where a bin entry's generated still (`png`) or moving preview (`webp`) lives.
fn bin_asset_path(st: &AppState, bin_id: &str, ext: &str) -> PathBuf {
    st.config.thumb_path(bin_id, ext)
}

/// A bin entry's still (`thumb.png`) or moving preview (`preview.webp`), both generated by
/// the `thumbs` job. 404 until then — which is what makes the still a fallback for the
/// preview rather than a second thing to keep in step.
async fn bin_asset(State(st): S, Path((id, file)): Path<(String, String)>) -> Result<Response, ApiError> {
    if !kuma_core::is_valid_id(&id) {
        return Err(ApiError::bad_request("bad bin id"));
    }
    let ext = match file.as_str() {
        "thumb.png" => "png",
        "preview.webp" => "webp",
        // Written with `strip.json`, which is what says it is current.
        "strip.jpg" => "strip.jpg",
        "peaks.json" | "strip.json" => return media_view(st, id, file).await,
        _ => return Err(ApiError::not_found("not found")),
    };
    file_response(&bin_asset_path(&st, &id, ext))
}

/// One media view generated at a time: a timeline full of new clips asks for all of theirs
/// at once, and ffmpeg per clip in parallel only makes every one of them late.
static MEDIA_VIEWS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the timeline draws on a media entry's clips, made on first request and kept in
/// `.kuma/thumbs/` beside the overlay stills (so a prune drops them with the entry):
/// `peaks.json`, its waveform, and `strip.json`, the index of its filmstrip `strip.jpg`.
/// Each names the file it was made from, so a relinked entry is made again. Looking, not
/// editing: nothing here touches the document.
async fn media_view(st: Arc<AppState>, id: String, file: String) -> Result<Response, ApiError> {
    let (path, kind, meta) = match read(&st.project).bin.get(&id) {
        Some(kuma_core::BinItem { source: BinSource::File { path, kind, .. }, meta, .. }) => (path.clone(), *kind, meta.clone().unwrap_or_default()),
        _ => return Err(ApiError::not_found("not a media entry")),
    };
    let src = jobs::resolve_path(&st, std::path::Path::new(&path));
    let out = bin_asset_path(&st, &id, &file);
    let target = out.clone();
    blocking(move || -> Result<(), String> {
        let source = src.to_string_lossy().into_owned();
        let stamp = std::fs::metadata(&src).and_then(|m| m.modified()).map_err(|e| format!("{path}: {e}"))?;
        // Current views are answered without waiting on one being made.
        if current(&out, &source, stamp) {
            return Ok(());
        }
        let _one = MEDIA_VIEWS.lock().unwrap_or_else(|p| p.into_inner());
        let _cache = read(&st.cache_users);
        if current(&out, &source, stamp) {
            return Ok(());
        }
        let dir = st.config.thumbs_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Scratch is `.tmp-…` beside its target until persisted, and gone on any error.
        let scratch = |suffix: &str| tempfile::Builder::new().prefix(".tmp-").suffix(suffix).tempfile_in(&dir).map_err(|e| e.to_string());
        let v = if file == "peaks.json" {
            json!({ "source": source, "rate": kuma_overlay::PEAK_RATE, "peaks": kuma_overlay::peaks(&src)? })
        } else {
            let jpg = scratch(".jpg")?;
            let strip = kuma_overlay::filmstrip(&src, &meta, kind == kuma_core::MediaKind::Image, jpg.path())?;
            jpg.persist(bin_asset_path(&st, &id, "strip.jpg")).map_err(|e| e.to_string())?;
            json!({ "source": source, "strip": strip })
        };
        let tmp = scratch(".json")?;
        std::fs::write(tmp.path(), v.to_string()).map_err(|e| e.to_string())?;
        tmp.persist(&out).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await?
    .map_err(ApiError::bad_request)?;
    file_response(&target)
}

/// Whether the view at `out` was made from `source` since it last changed.
fn current(out: &std::path::Path, source: &str, stamp: std::time::SystemTime) -> bool {
    #[derive(Deserialize)]
    struct Made {
        source: String,
    }
    std::fs::metadata(out).and_then(|m| m.modified()).is_ok_and(|t| t >= stamp)
        && std::fs::read(out).ok().and_then(|b| serde_json::from_slice::<Made>(&b).ok()).is_some_and(|m| m.source == source)
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
    parent: Option<String>,
    #[serde(default)]
    base_revision: Option<u64>,
    #[serde(default = "default_origin")]
    origin: String,
}

/// Probe a media file and add it to the bin. The id defaults to the file stem.
async fn add_media(State(st): S, Json(req): Json<AddMedia>) -> ApiResult {
    let (id, rev, warnings) = blocking(move || jobs::add_media(&st, &req.path, req.id, req.parent, req.base_revision, &req.origin)).await?.map_err(edit_or_bad_request)?;
    Ok(Json(json!({ "id": id, "revision": rev, "warnings": warnings })))
}

/// Speech to text on a media entry, as a job: whisper takes seconds to minutes. The
/// result names the new `subtitles` bin entry.
async fn transcribe(State(st): S, Json(req): Json<TranscribeRequest>) -> Json<Value> {
    let s2 = st.clone();
    Json(json!({ "job": jobs::spawn(st, "transcribe", move |progress| jobs::transcribe(&s2, &req, progress)) }))
}

// ---------- jobs ----------

/// `p` relative to the project when it is inside it: a path as a person would type it.
fn project_relative(st: &AppState, p: &std::path::Path) -> std::path::PathBuf {
    p.strip_prefix(st.config.project_root()).unwrap_or(p).to_path_buf()
}

/// `jobs::render_target` off the async threads: it can try a hardware encoder.
async fn target(st: &Arc<AppState>, req: &RenderRequest) -> Result<(String, &'static kuma_mlt::ExportPreset, PathBuf), ApiError> {
    let (st, req) = (st.clone(), req.clone());
    blocking(move || jobs::render_target(&st, &req)).await?.map_err(ApiError::bad_request)
}

/// Queue a render. Sequence and output are settled now, so the queue shows what will be
/// written whatever is opened meanwhile, and a bad preset is refused here.
async fn render(State(st): S, Json(mut req): Json<RenderRequest>) -> ApiResult {
    let (sequence, preset, output) = target(&st, &req).await?;
    let params = json!({
        "sequence": sequence, "preset": preset.id, "output": output, "path": project_relative(&st, &output),
        "threads": req.threads, "quality": req.quality, "speed": req.speed,
    });
    req.sequence = Some(sequence);
    req.output = Some(output);
    let s2 = st.clone();
    let id = jobs::spawn_cancellable(st, "render", params, move |progress, cancel| jobs::render(&s2, &req, progress, cancel));
    Ok(Json(json!({ "job": id })))
}

/// Where a render would write and what it would replace — a file there, or unfinished
/// renders to the same path — so a client can warn first; and how many frames it would
/// build at once, which is this machine's to say. Reads only.
async fn render_target(State(st): S, Query(req): Query<RenderRequest>) -> ApiResult {
    let (sequence, preset, output) = target(&st, &req).await?;
    let threads = req.threads.unwrap_or_else(kuma_mlt::melt::render_threads);
    Ok(Json(json!({
        "sequence": sequence, "preset": preset.id, "path": project_relative(&st, &output), "exists": output.exists(),
        "pending": st.jobs.writing(&output), "output": output, "threads": threads, "max_threads": kuma_mlt::presets::MAX_THREADS,
    })))
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
    Ok(Json(blocking(move || jobs::sample(&st, &req, &|_, _| {})).await??))
}

/// The same sample as a job, for a caller that must not block on the bakes it needs:
/// progress names the overlay baking, and the result is what `/api/sample` answers.
async fn sample_job(State(st): S, Json(req): Json<SampleRequest>) -> Json<Value> {
    let s2 = st.clone();
    let id = jobs::spawn(st, "sample", move |progress| jobs::sample(&s2, &req, progress));
    Json(json!({ "job": id }))
}

/// How loud a sequence is, as a cancellable job: it renders the sound, which takes a while
/// for a long video. The result is `jobs::measure_audio`'s.
async fn measure_audio(State(st): S, Json(req): Json<MeasureRequest>) -> Json<Value> {
    let s2 = st.clone();
    let params = json!({ "sequence": req.sequence, "mute": req.mute, "solo": req.solo });
    Json(json!({ "job": jobs::spawn_cancellable(st, "measure", params, move |progress, cancel| jobs::measure_audio(&s2, &req, progress, cancel)) }))
}

/// How loud one source is and where it is quiet, as a cancellable job: ffmpeg decodes the
/// whole stretch, which takes a while for a long recording. The result is
/// `jobs::measure_media`'s.
async fn measure_media(State(st): S, Json(req): Json<MediaMeasureRequest>) -> Json<Value> {
    let s2 = st.clone();
    let params = json!({ "bin": req.bin, "path": req.path, "range": req.range });
    Json(json!({ "job": jobs::spawn_cancellable(st, "measure-media", params, move |progress, cancel| jobs::measure_media(&s2, &req, progress, cancel)) }))
}

async fn cache_usage(State(st): S) -> ApiResult {
    Ok(Json(json!(blocking(move || crate::cache::usage(&st)).await?.map_err(anyhow::Error::from)?)))
}

#[derive(Deserialize)]
struct PruneReq {
    #[serde(default)]
    all: bool,
}

/// A job: it waits for renders using the cache.
async fn cache_prune(State(st): S, Json(req): Json<PruneReq>) -> Json<Value> {
    let s2 = st.clone();
    let id = jobs::spawn(st, "prune", move |progress| Ok(json!(crate::cache::prune(&s2, req.all, progress)?)));
    Json(json!({ "job": id }))
}

#[derive(Deserialize)]
struct SnapshotQuery {
    t: f64,
    #[serde(default)]
    width: Option<u32>,
}

/// One composited frame as PNG (melt), the pre-engine preview fallback.
async fn snapshot(State(st): S, Query(q): Query<SnapshotQuery>) -> Result<Response, ApiError> {
    // Same overlay scope as the engine preview this stands in for: nested sequences baked
    // in, the open sequence's own left to the live DOM the UI draws on top.
    let req = SampleRequest { t0: q.t, width: q.width.or(Some(960)), overlays: jobs::OverlayScope::Nested, sequence: Some(st.current_sequence()), ..Default::default() };
    let samples = blocking(move || jobs::sample_frames(&st, &req, &|_, _| {})).await??;
    let bytes = std::fs::read(&samples[0].path).map_err(|e| anyhow::anyhow!(e))?;
    Ok(([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")], bytes).into_response())
}

async fn job_get(State(st): S, Path(id): Path<u64>) -> ApiResult {
    st.jobs.get(id).map(|j| Json(json!(j))).ok_or(ApiError::not_found("no such job"))
}

#[derive(Deserialize)]
struct JobListQuery {
    kind: Option<String>,
}

/// The jobs still remembered, oldest first: events only tell a new window what happens next.
async fn job_list(State(st): S, Query(q): Query<JobListQuery>) -> Json<Value> {
    Json(json!({ "jobs": st.jobs.list(q.kind.as_deref()) }))
}

/// Clear finished jobs, for every window.
async fn job_clear(State(st): S) -> Json<Value> {
    let ids = st.jobs.clear_finished();
    let _ = st.events.send(crate::state::Event::JobsCleared { ids: ids.clone() });
    Json(json!({ "cleared": ids }))
}

async fn job_cancel(State(st): S, Path(id): Path<u64>) -> ApiResult {
    match jobs::cancel(&st, id) {
        Ok(job) => Ok(Json(json!(job))),
        Err(e @ jobs::CancelError::NotFound) => Err(ApiError::not_found(e)),
        Err(e) => Err(ApiError::new(StatusCode::CONFLICT, e)),
    }
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
    /// Hand the engine the timeline even when it is unchanged (the Reload button).
    #[serde(default)]
    force: bool,
}

async fn preview_reload(State(st): S, body: Option<Json<PreviewReload>>) -> ApiResult {
    let body = body.map(|b| b.0);
    if let Some(s) = body.as_ref().and_then(|b| b.scale) {
        *lock(&st.preview.scale) = s.clamp(0.25, 1.0);
    }
    let r = if body.is_some_and(|b| b.force) { preview::reload_forced(&st).await? } else { preview::reload(&st).await? };
    Ok(Json(json!({ "mlt": r.path, "changed": r.changed, "video": r.video, "audio": r.audio, "baked": r.baked, "baking": r.baking, "scale": *lock(&st.preview.scale) })))
}

// ---------- overlay preview (live DOM) ----------

/// An overlay clip, with its entry's folder and vars.
fn clip_overlay<'a>(project: &'a Project, clip: &str) -> Result<(&'a kuma_core::Clip, &'a str, &'a serde_json::Map<String, Value>), ApiError> {
    let (_, _, c) = project.find_clip(clip).ok_or(ApiError::not_found("no such clip"))?;
    match project.item(c).map(|i| &i.source) {
        Some(BinSource::Overlay { overlay, vars }) => Ok((c, overlay, vars)),
        _ => Err(ApiError::bad_request("not an overlay clip")),
    }
}

/// Serve an overlay with kuma's contract injected and the hyperframes runtime preloaded,
/// so the UI can drive it with `seek(t)`.
///
/// There are two ways in because an overlay is addressable twice over, exactly as the bin
/// splits it: a **clip** is a placement and is what decides how long the thing runs, while
/// the **entry** is what is shown and exists before anything places it — which is what the
/// overlay workbench iterates on. Both end in the same contract.
async fn overlay_preview(State(st): S, Path(clip): Path<String>) -> Result<Response, ApiError> {
    let project = read(&st.project).clone();
    // The bin entry decides the overlay and its vars; the clip decides how long it runs.
    let (c, overlay, vars) = clip_overlay(&project, &clip)?;
    let r = crate::overlays::resolve(&st, overlay).map_err(|e| ApiError::not_found(format!("{e:#}")))?;
    // Clip ids are validated to be URL-safe.
    let base = format!("/overlay-preview/{}/", c.id);
    let (vars, duration, speed, id) = (vars.clone(), c.duration() * c.speed.abs(), c.speed, c.id.clone());
    blocking(move || overlay_document(&st, &project, &r, &vars, duration, speed, Some(&id), &base, None)).await?
}

#[derive(Deserialize)]
struct EntryPreview {
    /// How long to render the entry for. An entry has no length of its own — a clip is
    /// what gives it one — so the workbench says which length it is looking at, and
    /// without one the overlay's own default applies.
    #[serde(default)]
    duration: Option<f64>,
    /// The frame to render into, when it is not the project's. An overlay is required to
    /// look right at 1920×1080 and 1080×1920 alike, so the workbench can ask for a frame
    /// the project is not in — which changes nothing about the project.
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

/// An overlay **bin entry**, previewed with no placement: the workbench edits what an
/// entry is, and that is editable before any clip plays it.
async fn overlay_entry(State(st): S, Path(id): Path<String>, Query(q): Query<EntryPreview>) -> Result<Response, ApiError> {
    let project = read(&st.project).clone();
    let view = jobs::entry_view(&st, &project, &id, q.duration, q.width, q.height)?;
    // Bin ids are validated to be URL-safe.
    let base = format!("/overlay-entry/{id}/");
    blocking(move || overlay_document(&st, &project, &view.overlay, &view.vars, view.duration, 1.0, None, &base, Some((view.width, view.height)))).await?
}



/// The overlay's markup with kuma's contract injected — the one place the live preview's
/// document is built, whichever of the two routes above asked for it. Blocking: it reads
/// the overlay and resolves fonts.
#[allow(clippy::too_many_arguments)]
fn overlay_document(
    st: &AppState,
    project: &Project,
    overlay: &crate::overlays::Resolved,
    vars: &serde_json::Map<String, Value>,
    duration: f64,
    speed: f64,
    clip: Option<&str>,
    base: &str,
    // The frame to render into; the project's own unless the caller is looking at another.
    frame: Option<(u32, u32)>,
) -> Result<Response, ApiError> {
    let (dir, manifest) = (&overlay.dir, &overlay.manifest);
    let html = std::fs::read_to_string(dir.join("index.html")).map_err(|e| anyhow::anyhow!(e))?;
    let head_extra = format!("<base href=\"{base}\">\n<script src=\"/vendor/hyperframe.runtime.iife.js\"></script>\n");
    let (width, height) = frame.unwrap_or((project.width, project.height));
    let vars = manifest.resolve_vars(vars);
    // The files the bake stages, from the route the picker uses.
    let fonts = st.baker.fonts(manifest, &vars);
    let faces = fonts.css(FONT_ROUTE);
    let out = kuma_overlay::Contract {
        vars: &vars,
        fps: project.fps,
        width,
        height,
        duration,
        speed,
        bake: false,
        clip,
        head_extra: &head_extra,
        fonts: kuma_overlay::FontCss { faces: &faces, vars: &fonts.vars },
    }
    .apply(&html);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], out).into_response())
}

/// Files referenced by a previewed overlay, from its folder (no manifest read).
async fn overlay_file(State(st): S, Path((clip, rest)): Path<(String, String)>) -> Result<Response, ApiError> {
    let dir = {
        let project = read(&st.project);
        let (_, overlay, _) = clip_overlay(&project, &clip)?;
        crate::overlays::folder(&st, overlay).map_err(ApiError::bad_request)?
    };
    serve_overlay_file(&st, &dir, &rest)
}

/// Files referenced by a previewed overlay entry, resolved against its folder.
async fn overlay_entry_file(State(st): S, Path((id, rest)): Path<(String, String)>) -> Result<Response, ApiError> {
    let project = read(&st.project).clone();
    let view = jobs::entry_view(&st, &project, &id, None, None, None)?;
    serve_overlay_file(&st, &view.overlay.dir, &rest)
}

/// A blueprint's files: its thumbnails for the browser.
async fn blueprint_static(State(st): S, Path((id, rest)): Path<(String, String)>) -> Result<Response, ApiError> {
    let dir = st.blueprint_dir(&id).map_err(ApiError::not_found)?;
    serve_overlay_file(&st, &dir, &rest)
}

/// A file inside an overlay folder. `_vendor/...` resolves exactly as the bake stages it.
fn serve_overlay_file(st: &AppState, dir: &std::path::Path, rest: &str) -> Result<Response, ApiError> {
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
        "otf" => "font/otf",
        _ => "application/octet-stream",
    };
    let mut resp = bytes.into_response();
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    Ok(resp)
}
