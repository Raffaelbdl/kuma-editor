//! Project state, persistence, undo, blueprints and event broadcast.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use kuma_core::edit::{self, EditError};
use kuma_core::manifest::{ManifestError, OverlayManifest};
use kuma_core::{Issue, Project, ValidationContext};
use kuma_overlay::{Baker, Transcriber};

use crate::history::{self, Change, History, Kind, Snapshot, Target};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct Config {
    /// Project file (`*.kuma.json`). Its directory is the project root.
    pub project_file: PathBuf,
    /// Repository root: `node_modules/hyperframes`, `ui/dist`, `engine/build`.
    pub repo_root: PathBuf,
    /// The asset libraries read. The project's own `presets/` and `fonts/` are searched
    /// after them; its `overlays/` is never a library.
    pub libraries: Libraries,
    pub bind: SocketAddr,
}

impl Config {
    pub fn project_root(&self) -> PathBuf {
        self.project_file.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.project_root().join(".kuma")
    }
    pub fn bakes_dir(&self) -> PathBuf {
        self.cache_dir().join("bakes")
    }
    pub fn samples_dir(&self) -> PathBuf {
        self.cache_dir().join("samples")
    }
    pub fn thumbs_dir(&self) -> PathBuf {
        self.cache_dir().join("thumbs")
    }
    /// Where a bin entry's generated files live: its still (`png`), moving preview
    /// (`webp`), or a media entry's waveform and filmstrip (`peaks.json`, `strip.…`).
    pub fn thumb_path(&self, bin_id: &str, ext: &str) -> PathBuf {
        self.thumbs_dir().join(format!("{bin_id}.{ext}"))
    }
    /// The bin id a thumbnail file belongs to (ids have no dots).
    pub fn thumb_owner(file_name: &str) -> &str {
        file_name.split('.').next().unwrap_or_default()
    }
    /// Every library's `blueprints/`, lowest precedence first. Never a project's.
    pub fn blueprint_dirs(&self) -> Vec<PathBuf> {
        self.libraries.dirs().iter().map(|d| d.join("blueprints")).collect()
    }
    /// Every library's shared `_vendor/`, highest precedence first, each consulted file by
    /// file (`Baker::vendor_layers`): one library's helpers never hide another's. Project
    /// overlays use them too.
    pub fn vendor_dirs(&self) -> Vec<PathBuf> {
        self.blueprint_dirs().into_iter().rev().map(|d| d.join("_vendor")).collect()
    }
    /// Where "Save as blueprint" writes: the user library, not a `KUMA_LIBRARY` entry.
    pub fn user_blueprint_dir(&self) -> PathBuf {
        self.libraries.user.join("blueprints")
    }
    /// Where `font` vars look, lowest precedence first: every library's `fonts/`, then the
    /// project's own, which wins — so a project carrying a font renders it wherever it is
    /// opened.
    pub fn font_dirs(&self) -> Vec<(PathBuf, kuma_overlay::Origin)> {
        use kuma_overlay::Origin;
        self.libraries.dirs().iter().map(|d| (d.join("fonts"), Origin::Library)).chain([(self.project_font_dir(), Origin::Project)]).collect()
    }
    /// The project's own fonts.
    pub fn project_font_dir(&self) -> PathBuf {
        self.project_root().join("fonts")
    }
    /// Where a font added "for every project" goes: the user library, as a saved blueprint does.
    pub fn user_font_dir(&self) -> PathBuf {
        self.libraries.user.join("fonts")
    }
    /// Where kuma puts the overlay folders it copies or scaffolds.
    pub fn overlays_dir(&self) -> PathBuf {
        self.project_root().join("overlays")
    }
    /// Every library's `presets/` plus the project's own, lowest precedence first.
    pub fn all_preset_dirs(&self) -> Vec<PathBuf> {
        self.libraries.dirs().iter().map(|d| d.join("presets")).chain([self.project_root().join("presets")]).collect()
    }
    /// Where installed design systems are looked for. Unlike blueprints and presets these
    /// are not read at runtime: one is **copied into the project** and the agent reads it
    /// there, so the project is the destination rather than the last search path entry.
    pub fn design_system_dirs(&self) -> Vec<PathBuf> {
        self.libraries.dirs().iter().map(|d| d.join("design-systems")).collect()
    }
    /// The design system this project carries, if it has been given one.
    pub fn project_design_system(&self) -> Option<PathBuf> {
        let d = self.project_root().join("design-system/DESIGN.md");
        d.is_file().then_some(d)
    }
    /// Design systems installed on the search path, by id: the folder and the title from
    /// its `DESIGN.md` (the first heading). Later directories win, as they do for
    /// blueprints. A folder without a `DESIGN.md` is not one.
    pub fn installed_design_systems(&self) -> BTreeMap<String, (PathBuf, String)> {
        let mut out = BTreeMap::new();
        for dir in self.design_system_dirs() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten().map(|e| e.path()) {
                let Ok(text) = std::fs::read_to_string(e.join("DESIGN.md")) else { continue };
                let Some(id) = e.file_name().and_then(|s| s.to_str()) else { continue };
                let title = text.lines().find_map(|l| l.strip_prefix("# ")).unwrap_or(id).trim().to_string();
                out.insert(id.to_string(), (e.clone(), title));
            }
        }
        out
    }
}

/// Copy a folder, recursively. Shared by the library install and the design-system copy;
/// the baker has its own because it also filters what it stages.
pub fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_files(src, dst, true).map(drop)
}

/// Copy only the files of `src` that `dst` lacks, at any depth. Returns how many.
pub fn add_missing(src: &Path, dst: &Path) -> std::io::Result<usize> {
    copy_files(src, dst, false)
}

fn copy_files(src: &Path, dst: &Path, overwrite: bool) -> std::io::Result<usize> {
    let mut n = 0;
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let target = dst.join(e.file_name());
        if e.path().is_dir() {
            n += copy_files(&e.path(), &target, overwrite)?;
        } else if overwrite || !target.exists() {
            std::fs::copy(e.path(), target)?;
            n += 1;
        }
    }
    Ok(n)
}

/// The user's asset library: `$XDG_DATA_HOME/kuma` (`~/.local/share/kuma`).
pub fn user_library() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    data.join("kuma")
}

/// The asset libraries a daemon reads, each with `blueprints/`, `presets/`, `fonts/` and
/// `design-systems/`, kept by where each comes from rather than as one list: what can be
/// taken off, and what stands in for what, is then never guessed back from a path.
#[derive(Debug, Clone)]
pub struct Libraries {
    /// The user library: where saved blueprints, shared fonts and whisper models go, and
    /// where the list of added libraries is kept.
    pub user: PathBuf,
    /// The checkout's starter set, standing in while the user library has no blueprints.
    pub starter: Option<PathBuf>,
    /// `KUMA_LIBRARY`'s, lowest precedence first.
    pub env: Vec<PathBuf>,
    /// The libraries added to the user library, as last read (`refresh`). Shared by clones.
    added: Arc<RwLock<Vec<PathBuf>>>,
}

impl Libraries {
    /// The user library, and what was added to it.
    pub fn new(user: PathBuf) -> Self {
        let libraries = Libraries { user, starter: None, env: Vec::new(), added: Arc::default() };
        libraries.refresh();
        libraries
    }

    /// What every daemon and command reads: the user library, `KUMA_LIBRARY`'s (a
    /// `:`-separated list), and — when that is unset and the user library holds no
    /// blueprints — the checkout's own `library/`, the starter set.
    ///
    /// The desktop shell has always done this for the daemons it starts (`libraryFallback`
    /// in `app/src/kuma.ts`, which also says so in the launcher). A daemon `kuma mcp` or
    /// `kuma serve` started did not, so before `kuma library` had been run it had no
    /// blueprints and none of the fonts they name: the same project drew its titles in
    /// fallback fonts or not, depending on which process had started the daemon.
    pub fn from_env(repo_root: &Path) -> Self {
        let mut libraries = Libraries::new(user_library());
        let asked = std::env::var_os("KUMA_LIBRARY");
        libraries.env = asked.iter().flat_map(std::env::split_paths).collect();
        libraries.starter = starter_set(&libraries.user, asked.is_some(), repo_root);
        libraries
    }

    /// Read the list of added libraries again: what a rescan does, so a library added by
    /// another daemon or by `kuma library add` is read from the next one.
    pub fn refresh(&self) {
        *write(&self.added) = added_libraries(&self.user);
    }

    /// Every library, lowest precedence first, with where it comes from: the user library
    /// and the starter set standing in for it, those added to the user library, then
    /// `KUMA_LIBRARY`'s. A library added and also named otherwise is read where it is named.
    pub fn sources(&self) -> Vec<(PathBuf, LibrarySource)> {
        let named = |d: &PathBuf| *d == self.user || self.starter.as_ref() == Some(d) || self.env.contains(d);
        std::iter::once((self.user.clone(), LibrarySource::User))
            .chain(self.starter.iter().map(|d| (d.clone(), LibrarySource::Starter)))
            .chain(read(&self.added).iter().filter(|d| !named(d)).map(|d| (d.clone(), LibrarySource::Added)))
            .chain(self.env.iter().map(|d| (d.clone(), LibrarySource::Env)))
            .collect()
    }

    /// `sources`, the folders alone.
    pub fn dirs(&self) -> Vec<PathBuf> {
        self.sources().into_iter().map(|(d, _)| d).collect()
    }
}

/// The checkout's `library/`, when nothing else was asked for and the user library has no
/// blueprints. Libraries added to the user library do not count: they are someone's own
/// blueprints beside kuma's, which still need kuma's titles and runtime.
fn starter_set(user: &Path, asked: bool, repo_root: &Path) -> Option<PathBuf> {
    let installed = std::fs::read_dir(user.join("blueprints")).is_ok_and(|mut d| d.next().is_some());
    let shipped = repo_root.join("library");
    (!asked && !installed && shipped.join("blueprints").is_dir()).then_some(shipped)
}

/// Where the user library lists the libraries added to it: `libraries.json`,
/// `{ "libraries": ["/abs/path", …] }`, lowest precedence first. It lives in the user library
/// so every daemon on the machine reads the same list, whoever started it.
pub fn added_libraries_file(user: &Path) -> PathBuf {
    user.join("libraries.json")
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AddedLibraries {
    libraries: Vec<PathBuf>,
}

/// The libraries added to the user library `user`, as listed; none when there is no list,
/// and none, with a warning, when it does not parse.
pub fn added_libraries(user: &Path) -> Vec<PathBuf> {
    let file = added_libraries_file(user);
    let Ok(text) = std::fs::read_to_string(&file) else { return Vec::new() };
    match serde_json::from_str::<AddedLibraries>(&text) {
        Ok(l) => l.libraries,
        Err(e) => {
            tracing::warn!("{} is not a library list, so no added library is read: {e}", file.display());
            Vec::new()
        }
    }
}

fn write_added_libraries(user: &Path, libraries: Vec<PathBuf>) -> anyhow::Result<()> {
    crate::jobs::write_atomic(&added_libraries_file(user), (serde_json::to_string_pretty(&AddedLibraries { libraries })? + "\n").as_bytes())
}

/// What a library holds, by folder: anything else is not one.
pub const LIBRARY_KINDS: [&str; 4] = ["blueprints", "presets", "fonts", "design-systems"];

/// Add `dir` to the libraries the user library `user` lists, last, so it wins over the ones
/// added before it. Its `blueprints/` folder names the library too. Answers with the folder
/// as it is listed (absolute), and whether it was new.
pub fn add_library(user: &Path, dir: &Path) -> anyhow::Result<(PathBuf, bool)> {
    let mut dir = dir.canonicalize().map_err(|e| anyhow::anyhow!("cannot open {}: {e}", dir.display()))?;
    anyhow::ensure!(dir.is_dir(), "{} is not a folder", dir.display());
    if dir.file_name().is_some_and(|n| n == "blueprints") && !LIBRARY_KINDS.iter().any(|k| dir.join(k).is_dir()) {
        dir.pop();
    }
    anyhow::ensure!(
        LIBRARY_KINDS.iter().any(|k| dir.join(k).is_dir()),
        "{} is not a kuma library: it has none of {}/",
        dir.display(),
        LIBRARY_KINDS.join("/, ")
    );
    anyhow::ensure!(user.canonicalize().ok().as_ref() != Some(&dir), "{} is your own library, which is always read", dir.display());
    let mut listed = added_libraries(user);
    if listed.contains(&dir) {
        return Ok((dir, false));
    }
    listed.push(dir.clone());
    write_added_libraries(user, listed)?;
    Ok((dir, true))
}

/// Take `dir` off the list. Nothing in it is touched, and nothing a project copied from it.
/// Refused for a folder the list does not have: the user library, the starter set and
/// `KUMA_LIBRARY`'s are not taken off here.
pub fn remove_library(user: &Path, dir: &Path) -> anyhow::Result<()> {
    let mut listed = added_libraries(user);
    let before = listed.len();
    let canonical = dir.canonicalize().ok();
    listed.retain(|d| d != dir && Some(d) != canonical.as_ref());
    anyhow::ensure!(listed.len() < before, "{} is not an added library (`kuma library list` says where each comes from)", dir.display());
    write_added_libraries(user, listed)
}

/// Where a library on the search path comes from, which says whether it can be taken off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySource {
    /// The user library (`~/.local/share/kuma`), where saved blueprints and shared fonts go.
    User,
    /// The checkout's starter set, read while the user library has no blueprints.
    Starter,
    /// Added to the user library's list (`kuma library add`, the Blueprints panel).
    Added,
    /// Named by `KUMA_LIBRARY`.
    Env,
}

impl LibrarySource {
    /// What a library is called where it is listed: its folder's name, or what it is.
    pub fn name(self, path: &Path) -> String {
        match self {
            LibrarySource::User => "My library".into(),
            LibrarySource::Starter => "Starter set".into(),
            _ => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string()),
        }
    }
}

/// A library as the Blueprints panel lists it.
#[derive(Debug, Serialize)]
pub struct LibraryInfo {
    pub path: PathBuf,
    /// Its folder's name, or what it is for the user library and the starter set.
    pub name: String,
    pub source: LibrarySource,
    /// Listed, but not there (a drive not mounted): read as empty until it is.
    pub missing: bool,
}

/// Events pushed to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The project changed; clients should refetch.
    ProjectChanged { revision: u64, origin: String },
    Job(crate::jobs::Job),
    /// Finished jobs were cleared from the table; clients drop them too.
    JobsCleared { ids: Vec<u64> },
    /// An agent's tool call started or ended (`crate::activity`).
    /// Boxed: a record is several times the size of every other event.
    Activity(Box<crate::activity::Record>),
    /// A revision was made, or a patch refused (`crate::history`).
    History(crate::history::Entry),
    /// The marker list, whole and as `GET /api/markers` lists it, sent whenever it can
    /// have changed: a marker edited (by an edit, a step or a restore), answered, ticked or
    /// unticked. At most one per operation, after its `history`.
    Markers { markers: Vec<crate::markers::MarkerInfo> },
}

/// Documents to step back and forward to, each with the origin of the edit that left it.
/// Snapshots, shared with the history's: the document a step leaves is the one the history
/// kept when it was made, so it costs nothing more to keep here.
pub struct UndoStack {
    pub past: Vec<(Snapshot, String)>,
    pub future: Vec<(Snapshot, String)>,
}

/// The blueprints installed in the libraries, by folder name.
pub struct Blueprints {
    pub loaded: BTreeMap<String, (PathBuf, OverlayManifest)>,
    /// Manifests that failed to load, so the UI and the agent see why a blueprint is missing.
    pub errors: Vec<ManifestError>,
}

pub struct AppState {
    pub config: Config,
    pub project: RwLock<Project>,
    /// Held for the whole of every mutation (check → validate → swap → save) so two
    /// edits against the same base revision cannot both succeed.
    edit: Mutex<()>,
    pub undo: Mutex<UndoStack>,
    pub blueprints: RwLock<Blueprints>,
    pub baker: Baker,
    /// whisper.cpp, for the `transcribe` job. Discovered like the baker; absent is a job
    /// error naming what to install, never a daemon that will not start.
    pub transcriber: Transcriber,
    pub events: broadcast::Sender<Event>,
    pub jobs: crate::jobs::Jobs,
    pub preview: crate::preview::PreviewHandle,
    /// Which sequence is open for editing and preview. Session state, not document state:
    /// it is nobody's edit, it must not bump the revision or land in the undo stack, and a
    /// project reopened tomorrow should start on its master. `None` follows the master.
    open_sequence: Mutex<Option<kuma_core::BinId>>,
    /// Shared by renders, samples, bakes and thumbnails while they read the cache;
    /// exclusive for a prune.
    pub cache_users: RwLock<()>,
    /// Every revision made and patch refused, and recent documents to restore. Appended
    /// under the edit lock, where the change happens, so its order is the revisions' order.
    pub history: Mutex<History>,
    /// The agent's recent tool calls, as the MCP server reports them.
    pub activity: Mutex<crate::activity::Activity>,
    /// Which markers are done, by id. Session state like the history it is derived from.
    answers: Mutex<crate::markers::Answers>,
}

/// Why a revision could not be restored.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("revision {0} is not kept: only the last {n} revisions this daemon made can be restored", n = crate::history::SNAPSHOTS)]
    Unknown(u64),
    #[error(transparent)]
    Edit(#[from] EditError),
}

/// What an edit records about itself beyond its origin.
#[derive(Default)]
pub struct Step<'a> {
    pub base_revision: Option<u64>,
    /// The markers it answers, by id.
    pub answers: &'a [String],
}

/// An edit's result as `AppState::edit` wants it from the closure that makes it: the
/// document, and what the history says it touched and changed.
struct Edited {
    outcome: edit::EditOutcome,
    targets: Vec<Target>,
    changes: Vec<Change>,
    ops: usize,
}

impl Edited {
    /// A patch's, made from `before`: what it names, and how those changed.
    fn patch(before: &Project, outcome: edit::EditOutcome, ops: &[json_patch::PatchOperation]) -> Edited {
        let targets = history::patch_targets(before, Some(&outcome.project), ops);
        let changes = history::changes(before, &outcome.project, &targets);
        Edited { outcome, targets, changes, ops: ops.len() }
    }
}

#[derive(Debug, Serialize)]
pub struct BlueprintInfo {
    pub id: String,
    /// The library it comes from (`LibraryInfo::path`).
    pub library: PathBuf,
    pub manifest: OverlayManifest,
}

/// The library a blueprint folder (`<library>/blueprints/<id>`) belongs to.
fn library_of(blueprint: &Path) -> PathBuf {
    blueprint.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default()
}

/// Locks guard data that is only ever swapped whole, so a panic while holding one leaves
/// nothing half-updated: keep serving instead of bricking the daemon.
pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
pub fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}
pub fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

impl AppState {
    pub fn load(config: Config) -> anyhow::Result<Self> {
        let project = if config.project_file.is_file() {
            let text = std::fs::read_to_string(&config.project_file)?;
            // Through `parse_project`, so a file an older kuma wrote is brought forward
            // rather than refused by the validator for its version alone.
            kuma_core::parse_project(&text)?
        } else {
            Project::new_default(Project::name_from_path(&config.project_file), 1920, 1080, kuma_core::Fps::default())
        };
        std::fs::create_dir_all(config.cache_dir())?;
        // What the libraries give the baker is set by the first scan, below.
        let baker = Baker::discover(&config.repo_root, config.bakes_dir());
        let transcriber = Transcriber::discover(&config.repo_root, config.libraries.user.join("models"));
        let (events, _) = broadcast::channel(256);
        let st = AppState {
            blueprints: RwLock::new(Blueprints { loaded: BTreeMap::new(), errors: Vec::new() }),
            project: RwLock::new(project),
            edit: Mutex::new(()),
            undo: Mutex::new(UndoStack { past: Vec::new(), future: Vec::new() }),
            baker,
            transcriber,
            events,
            jobs: crate::jobs::Jobs::default(),
            preview: crate::preview::PreviewHandle::default(),
            open_sequence: Mutex::new(None),
            cache_users: RwLock::new(()),
            history: Mutex::new(History::default()),
            activity: Mutex::new(Default::default()),
            answers: Mutex::new(Default::default()),
            config,
        };
        // The revision it opened at can be restored like any it makes.
        let opened = {
            let p = read(&st.project);
            (p.revision, history::snapshot(&p)?)
        };
        lock(&st.history).keep(opened.0, opened.1);
        st.reload_blueprints();
        if !st.config.project_file.is_file() {
            st.save()?;
        }
        Ok(st)
    }

    /// Later libraries win on an id clash.
    pub fn reload_blueprints(&self) {
        // The libraries may have changed since the last scan: the baker follows them.
        self.config.libraries.refresh();
        self.baker.set_library(self.config.vendor_dirs(), self.config.font_dirs());
        let mut all = BTreeMap::new();
        let mut errors = Vec::new();
        for dir in self.config.blueprint_dirs() {
            let (loaded, errs) = OverlayManifest::load_all(&dir);
            all.extend(loaded);
            errors.extend(errs);
        }
        for e in &errors {
            tracing::warn!("blueprint skipped: {e}");
        }
        // Projects do not need blueprints, but the browser and subtitles do.
        if all.is_empty() {
            tracing::warn!("no blueprints loaded from {:?}; run `kuma library` to install the starter set", self.config.blueprint_dirs());
        }
        *write(&self.blueprints) = Blueprints { loaded: all, errors };
    }

    /// The folder of an installed blueprint.
    pub fn blueprint_dir(&self, id: &str) -> anyhow::Result<PathBuf> {
        read(&self.blueprints).loaded.get(id).map(|(dir, _)| dir.clone()).ok_or_else(|| anyhow::anyhow!("no blueprint `{id}` in the library; `kuma library` installs the starter set"))
    }

    pub fn blueprint_infos(&self) -> Vec<BlueprintInfo> {
        read(&self.blueprints)
            .loaded
            .iter()
            .map(|(id, (dir, m))| BlueprintInfo { id: id.clone(), library: library_of(dir), manifest: m.clone() })
            .collect()
    }

    /// The libraries read, highest precedence last.
    pub fn library_infos(&self) -> Vec<LibraryInfo> {
        self.config.libraries.sources().into_iter().map(|(path, source)| LibraryInfo { missing: !path.is_dir(), name: source.name(&path), path, source }).collect()
    }

    /// The sequence being edited. A chosen one that has since been deleted (or turned into
    /// something that is not a sequence) falls back to the master rather than failing every
    /// compile until someone notices — and is *forgotten*, so that re-creating an id by
    /// that name (an undo, an agent re-adding it) does not silently reopen it here while
    /// every client is still showing the master.
    pub fn current_sequence(&self) -> kuma_core::BinId {
        let p = read(&self.project);
        let mut open = lock(&self.open_sequence);
        if open.as_deref().is_some_and(|id| p.seq(id).is_none()) {
            *open = None;
        }
        open.clone().unwrap_or_else(|| p.master.clone())
    }

    /// Open a sequence for editing. `None` returns to following the master.
    pub fn open_sequence(&self, id: Option<kuma_core::BinId>) -> anyhow::Result<kuma_core::BinId> {
        if let Some(id) = &id {
            let p = read(&self.project);
            anyhow::ensure!(p.seq(id).is_some(), "`{id}` is not a sequence in this project");
        }
        *lock(&self.open_sequence) = id;
        Ok(self.current_sequence())
    }

    /// What `validate_project` and `kuma validate` report: the document's issues, and the
    /// text its overlay entries will not be drawn in (`fonts::check`).
    pub fn validate(&self, p: &Project) -> Vec<Issue> {
        let mut issues = kuma_core::validate(p, &self.validation_context());
        issues.extend(crate::fonts::check(self, p, &p.bin.keys().map(String::as_str).collect::<Vec<_>>()));
        issues
    }

    pub fn validation_context(&self) -> ValidationContext {
        // Files are checked, so a missing overlay folder is an error now, not at render.
        ValidationContext { root: self.config.project_root(), check_files: true }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = read(&self.project).clone();
        self.write_file(&p)
    }

    fn write_file(&self, p: &Project) -> anyhow::Result<()> {
        let tmp = self.config.project_file.with_extension("json.tmp");
        std::fs::write(&tmp, kuma_core::project_text(p)?)?;
        std::fs::rename(&tmp, &self.config.project_file)?;
        Ok(())
    }

    /// Persist `next`, then make it current, handing back the document it replaced. Nothing
    /// changes in memory if the write fails, so a failed edit never leaves clients on a
    /// revision the disk lacks.
    fn commit(&self, next: Project, origin: &str) -> anyhow::Result<Project> {
        self.write_file(&next)?;
        let rev = next.revision;
        let prev = std::mem::replace(&mut *write(&self.project), next);
        let _ = self.events.send(Event::ProjectChanged { revision: rev, origin: origin.to_string() });
        Ok(prev)
    }

    fn edit<F>(&self, origin: &str, kind: Kind, step: &Step, f: F) -> Result<(u64, Vec<Issue>), EditError>
    where
        F: FnOnce(&Project) -> Result<Edited, EditError>,
    {
        let _guard = lock(&self.edit);
        // Read in place, not copied: `commit` is the only writer and runs under the edit
        // lock held here, so the read lock blocks nobody while `f` runs. Let go before the
        // commit takes the write lock.
        let current = read(&self.project);
        let Edited { mut outcome, targets, changes, ops } = f(&current)?;
        // A step restores a document as it was; an edit's new markers say who left them,
        // in what is saved and what the history keeps.
        if kind == Kind::Edit {
            crate::markers::stamp(&current, &mut outcome.project, origin);
        }
        let markers_differ = crate::markers::differ(&current, &outcome.project);
        drop(current);
        let rev = outcome.project.revision;
        let next = history::snapshot(&outcome.project).map_err(|e| EditError::Patch(e.to_string()))?;
        let prev = self.commit(outcome.project, origin).map_err(|e| EditError::Patch(e.to_string()))?;
        let prev = self.kept(&prev);
        let entry = history::Entry { ops, base_revision: step.base_revision, changes, answers: step.answers.to_vec(), ..history::Entry::new(kind, origin, Some(rev), &targets) };
        self.landed(rev, next, entry, markers_differ, |u| {
            u.past.push((prev, origin.to_string()));
            if u.past.len() > 200 {
                u.past.remove(0);
            }
            u.future.clear();
        });
        Ok((rev, outcome.warnings))
    }

    /// `p`, a document just replaced, as the snapshot the history kept when it landed — it
    /// was current, so it is the newest kept — or serialised afresh should it not be there.
    fn kept(&self, p: &Project) -> Snapshot {
        let kept = lock(&self.history).snapshot(p.revision);
        // It was written to disk as JSON when it landed, so it serialises.
        kept.unwrap_or_else(|| history::snapshot(p).expect("a committed document serialises"))
    }

    /// What every step does once committed at `revision`, the history's entry and whether
    /// the markers changed worked out from the two documents before: `push` onto the undo
    /// stack, record `entry`, send the markers if they can have changed, keep `next` (the
    /// committed document) to restore.
    fn landed(&self, revision: u64, next: Snapshot, entry: history::Entry, markers_differ: bool, push: impl FnOnce(&mut UndoStack)) {
        push(&mut lock(&self.undo));
        let answered = self.record(entry);
        if answered || markers_differ {
            self.send_markers(&lock(&self.answers));
        }
        lock(&self.history).keep(revision, next);
    }

    /// Append to the history and tell every client; an entry naming markers answers them.
    /// Returns whether it did, for the caller to send the markers once with whatever else
    /// changed them.
    fn record(&self, entry: history::Entry) -> bool {
        let answering = entry.revision.filter(|_| !entry.answers.is_empty());
        if let Some(revision) = answering {
            let mut answers = lock(&self.answers);
            for id in &entry.answers {
                answers.insert(id.clone(), crate::markers::Done { revision, by: entry.author.clone() });
            }
        }
        lock(&self.history).push(entry.clone());
        let _ = self.events.send(Event::History(entry));
        answering.is_some()
    }

    /// Every marker, the master's first, each with who left it and whether it is done:
    /// `GET /api/markers`.
    pub fn marker_list(&self) -> Vec<crate::markers::MarkerInfo> {
        let answers = lock(&self.answers);
        crate::markers::list(&read(&self.project), &answers)
    }

    /// Broadcast the marker list. Built and sent while `answers` (its lock) is held, so
    /// whichever list is sent last was built last and clients end on the current one. The
    /// answers lock is always taken before the project's.
    fn send_markers(&self, answers: &crate::markers::Answers) {
        let markers = crate::markers::list(&read(&self.project), answers);
        let _ = self.events.send(Event::Markers { markers });
    }

    /// A patch, naming the markers it answers (none: `&[]`). A refusal is recorded too: the
    /// person sees what was tried and why it did not land, not only what did.
    pub fn apply_patch(&self, patch: &[json_patch::PatchOperation], base: Option<u64>, origin: &str, answers: &[String]) -> Result<(u64, Vec<Issue>), EditError> {
        let step = Step { base_revision: base, answers };
        let ctx = self.validation_context();
        let applied = self.edit(origin, Kind::Edit, &step, |p| Ok(Edited::patch(p, edit::apply_patch(p, base, patch, &ctx)?, patch)));
        if let Err(e) = &applied {
            let targets = history::patch_targets(&read(&self.project), None, patch);
            // Answers nothing: it made no revision.
            self.record(history::Entry { ops: patch.len(), base_revision: base, error: Some(history::refusal(e)), answers: answers.to_vec(), ..history::Entry::new(Kind::Refused, origin, None, &targets) });
        }
        applied
    }

    /// Compose a patch *from the project as it is now* and apply it, both under the edit
    /// lock. For work that takes minutes before it knows what to write — a transcription —
    /// where deciding against a snapshot taken at the start would race every edit made
    /// meanwhile. `build` must stay cheap: it runs with the lock held. `base` is checked
    /// like any patch's.
    pub fn apply_built<T>(&self, origin: &str, base: Option<u64>, build: impl FnOnce(&Project) -> anyhow::Result<(Vec<json_patch::PatchOperation>, T)>) -> anyhow::Result<(T, u64, Vec<Issue>)> {
        let mut carried = None;
        let step = Step { base_revision: base, ..Default::default() };
        let (rev, warnings) = self.edit(origin, Kind::Edit, &step, |p| {
            let (ops, value) = build(p).map_err(|e| EditError::Patch(format!("{e:#}")))?;
            carried = Some(value);
            Ok(Edited::patch(p, edit::apply_patch(p, base, &ops, &self.validation_context())?, &ops))
        })?;
        Ok((carried.expect("build ran"), rev, warnings))
    }

    /// Make the document what it was at `revision`, as one new edit: validated, undoable,
    /// and at a new revision number, so no client mistakes it for the old one — the stance
    /// undo takes. `origin` says who asked (`ui`, `mcp`).
    pub fn restore(&self, revision: u64, origin: Option<&str>) -> Result<(u64, Vec<Issue>), RestoreError> {
        // Copied out (a shared handle) and parsed after the history is let go of.
        let snap = lock(&self.history).snapshot(revision).ok_or(RestoreError::Unknown(revision))?;
        let doc = history::parse(&snap).map_err(|e| EditError::Patch(e.to_string()))?;
        drop(snap);
        let label = format!("{}: restore r{revision}", origin.unwrap_or("other"));
        let ctx = self.validation_context();
        Ok(self.edit(&label, Kind::Restore, &Step::default(), |p| {
            // `replace_project` sets the revision past the current one, whatever the snapshot says.
            let outcome = edit::replace_project(p, None, doc, &ctx)?;
            let (targets, changes) = history::diff(p, &outcome.project);
            Ok(Edited { targets, changes, ops: 1, outcome })
        })?)
    }

    /// Tick a marker done by hand (the current revision, by the user), or untick it, which
    /// forgets an edit's answer too, and send the list. `false` when there is no such marker.
    /// Tick a marker done, or untick it. `origin` says who (`ui`, `cli: …`), read as an
    /// edit's author is, so a tick from anywhere but the editor is not taken for a person's.
    pub fn tick_marker(&self, id: &str, done: bool, origin: &str) -> bool {
        let mut answers = lock(&self.answers);
        let revision = {
            let p = read(&self.project);
            if !crate::markers::exists(&p, id) {
                return false;
            }
            p.revision
        };
        if done {
            answers.insert(id.to_string(), crate::markers::Done { revision, by: crate::history::author(origin).into() });
        } else {
            answers.remove(id);
        }
        self.send_markers(&answers);
        true
    }

    /// Undo/redo swap whole snapshots; the revision keeps increasing so clients never
    /// confuse an undone state with an older one. `Ok(None)` means nothing to undo/redo.
    /// `by` is the origin of the request (`ui`, `mcp`), which the history names as the
    /// step's author; the step itself is the same either way.
    pub fn undo(&self, by: Option<&str>) -> anyhow::Result<Option<u64>> {
        self.swap(false, by)
    }

    pub fn redo(&self, by: Option<&str>) -> anyhow::Result<Option<u64>> {
        self.swap(true, by)
    }

    fn swap(&self, redo: bool, by: Option<&str>) -> anyhow::Result<Option<u64>> {
        let _guard = lock(&self.edit);
        // The undo stack is only ever changed under the edit lock, so it can be let go of
        // between the pop and the push without anyone seeing the step missing.
        let popped = {
            let mut u = lock(&self.undo);
            if redo { u.future.pop() } else { u.past.pop() }
        };
        let Some((snap, origin)) = popped else { return Ok(None) };
        let verb = if redo { "redo" } else { "undo" };
        let label = format!("{verb}:{origin}");
        // Worked out against the current document in place (as `edit` does), then committed.
        let ready = (|| {
            let current = read(&self.project);
            let mut restored = history::parse(&snap)?;
            restored.revision = current.revision + 1;
            let (targets, changes) = history::diff(&current, &restored);
            let markers_differ = crate::markers::differ(&current, &restored);
            drop(current);
            anyhow::Ok((history::snapshot(&restored)?, restored, targets, changes, markers_differ))
        })();
        let (next, restored, targets, changes, markers_differ) = match ready {
            Ok(r) => r,
            Err(e) => return Err(self.unpop(redo, snap, origin, e)),
        };
        let rev = restored.revision;
        let now = match self.commit(restored, &label) {
            Ok(now) => self.kept(&now),
            // A failed save changed nothing; the history step must survive it.
            Err(e) => return Err(self.unpop(redo, snap, origin, e)),
        };
        let entry = history::Entry {
            author: by.map(history::author).unwrap_or("other").to_string(),
            summary: format!("{verb}: {}", history::summary(&origin)),
            changes,
            ..history::Entry::new(if redo { Kind::Redo } else { Kind::Undo }, &label, Some(rev), &targets)
        };
        self.landed(rev, next, entry, markers_differ, |u| if redo { &mut u.past } else { &mut u.future }.push((now, origin)));
        Ok(Some(rev))
    }

    /// Put a step that did not land back where it was popped from, and pass its error on.
    fn unpop(&self, redo: bool, snap: Snapshot, origin: String, e: anyhow::Error) -> anyhow::Error {
        let mut u = lock(&self.undo);
        if redo { &mut u.future } else { &mut u.past }.push((snap, origin));
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first library is the user's, the rest `KUMA_LIBRARY`'s.
    fn config(dir: &Path, libraries: Vec<PathBuf>) -> Config {
        let mut dirs = libraries.into_iter();
        let mut libraries = Libraries::new(dirs.next().unwrap_or_else(|| dir.join("user")));
        libraries.env = dirs.collect();
        Config { project_file: dir.join("p/t.kuma.json"), repo_root: dir.to_path_buf(), libraries, bind: "127.0.0.1:0".parse().unwrap() }
    }

    /// Later libraries shadow earlier ones by id, the title is the first heading, and a
    /// folder without a `DESIGN.md` is not a design system.
    #[test]
    fn design_systems_are_found_by_their_design_md() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        for (lib, id, title) in [(&a, "studio", "Studio"), (&a, "old", "Old"), (&b, "studio", "Studio (mine)")] {
            let d = lib.join("design-systems").join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("DESIGN.md"), format!("# {title}\n\nprose")).unwrap();
        }
        std::fs::create_dir_all(a.join("design-systems/not-one")).unwrap();

        let found = config(tmp.path(), vec![a.clone(), b.clone()]).installed_design_systems();
        assert_eq!(found.keys().cloned().collect::<Vec<_>>(), ["old", "studio"]);
        assert_eq!(found["studio"].1, "Studio (mine)", "the later library wins");
        assert_eq!(found["studio"].0, b.join("design-systems/studio"));
        assert!(config(tmp.path(), vec![a]).project_design_system().is_none());
    }

    /// A daemon reads the starter set only while the user has no library of their own and
    /// asked for none: never shadowing blueprints they installed and edited.
    #[test]
    fn the_starter_set_stands_in_for_a_missing_library() {
        let tmp = tempfile::tempdir().unwrap();
        let (user, repo) = (tmp.path().join("user"), tmp.path().join("repo"));
        std::fs::create_dir_all(repo.join("library/blueprints/title")).unwrap();
        let shipped = Some(repo.join("library"));
        let starter = |repo: &Path, asked: bool| starter_set(&user, asked, repo);
        assert_eq!(starter(&repo, false), shipped, "nothing installed");
        assert_eq!(starter(&repo, true), None, "KUMA_LIBRARY names the libraries");
        std::fs::create_dir_all(user.join("blueprints")).unwrap();
        assert_eq!(starter(&repo, false), shipped, "an empty folder is not a library");
        std::fs::create_dir_all(user.join("blueprints/mine")).unwrap();
        assert_eq!(starter(&repo, false), None, "the user's own library");
        assert_eq!(starter(&tmp.path().join("elsewhere"), false), None, "no checkout, nothing to fall back on");
    }

    /// An added library sits after the user library and the starter set standing in for
    /// it, before `KUMA_LIBRARY`'s, and is read from the next rescan; a folder with nothing
    /// of a library's in it is refused. Where each comes from is kept, never read off a
    /// path: `KUMA_LIBRARY` naming the checkout's `library/` is still `KUMA_LIBRARY`'s.
    #[test]
    fn added_libraries_come_after_the_user_library() {
        let tmp = tempfile::tempdir().unwrap();
        let (user, repo, env) = (tmp.path().join("user"), tmp.path().to_path_buf(), tmp.path().join("env"));
        let house = tmp.path().join("house");
        std::fs::create_dir_all(house.join("presets")).unwrap();
        std::fs::create_dir_all(tmp.path().join("empty")).unwrap();
        assert!(add_library(&user, &tmp.path().join("empty")).is_err());
        assert!(add_library(&user, &tmp.path().join("nowhere")).is_err());
        let (dir, added) = add_library(&user, &house).unwrap();
        assert!(added && dir == house.canonicalize().unwrap());
        assert!(!add_library(&user, &house).unwrap().1, "once");

        let starter = repo.join("library");
        let mut libraries = Libraries::new(user.clone());
        libraries.starter = Some(starter.clone());
        libraries.env = vec![env.clone()];
        assert_eq!(libraries.dirs(), [user.clone(), starter.clone(), dir.clone(), env.clone()]);
        let sources: Vec<_> = libraries.sources().into_iter().map(|(_, s)| s).collect();
        assert_eq!(sources, [LibrarySource::User, LibrarySource::Starter, LibrarySource::Added, LibrarySource::Env]);
        let named = Libraries { starter: None, env: vec![starter.clone()], ..libraries.clone() };
        assert_eq!(named.sources()[2], (starter, LibrarySource::Env));

        remove_library(&user, &house).unwrap();
        assert!(remove_library(&user, &house).is_err(), "not listed any more");
        assert_eq!(libraries.dirs().len(), 4, "until the next rescan");
        libraries.refresh();
        assert!(!libraries.dirs().contains(&dir));
        assert!(house.join("presets").is_dir());
    }

    #[test]
    fn copy_tree_copies_nested_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("fonts")).unwrap();
        std::fs::write(src.join("DESIGN.md"), "# X").unwrap();
        std::fs::write(src.join("fonts/a.woff2"), b"\0").unwrap();
        let dst = tmp.path().join("p/design-system");
        copy_tree(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("DESIGN.md")).unwrap(), "# X");
        assert!(dst.join("fonts/a.woff2").is_file());
        // And the project now has one.
        let cfg = config(tmp.path(), vec![]);
        assert_eq!(cfg.project_design_system(), Some(dst.join("DESIGN.md")));
    }
}
