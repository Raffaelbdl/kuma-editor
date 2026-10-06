//! Project state, persistence, undo, blueprints and event broadcast.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};

use kuma_core::edit::{self, EditError};
use kuma_core::manifest::{ManifestError, OverlayManifest};
use kuma_core::{Issue, Project, ValidationContext};
use kuma_overlay::{Baker, Transcriber};

use crate::history::{self, History, Kind, Target};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct Config {
    /// Project file (`*.kuma.json`). Its directory is the project root.
    pub project_file: PathBuf,
    /// Repository root: `node_modules/hyperframes`, `ui/dist`, `engine/build`.
    pub repo_root: PathBuf,
    /// Asset libraries (each holds `blueprints/` and `presets/`), lowest precedence first.
    /// The project's own `presets/` is searched last; its `overlays/` is never a library.
    pub library_dirs: Vec<PathBuf>,
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
        self.library_dirs.iter().map(|d| d.join("blueprints")).collect()
    }
    /// Where "Save as blueprint" writes: the user library, not a `KUMA_LIBRARY` entry.
    pub fn user_blueprint_dir(&self) -> PathBuf {
        self.library_dirs[0].join("blueprints")
    }
    /// Where `font` vars look, lowest precedence first: every library's `fonts/`, then the
    /// project's own, which wins — so a project carrying a font renders it wherever it is
    /// opened.
    pub fn font_dirs(&self) -> Vec<(PathBuf, kuma_overlay::Origin)> {
        use kuma_overlay::Origin;
        self.library_dirs.iter().map(|d| (d.join("fonts"), Origin::Library)).chain([(self.project_font_dir(), Origin::Project)]).collect()
    }
    /// The project's own fonts.
    pub fn project_font_dir(&self) -> PathBuf {
        self.project_root().join("fonts")
    }
    /// Where a font added "for every project" goes: the user library, as a saved blueprint does.
    pub fn user_font_dir(&self) -> PathBuf {
        self.library_dirs[0].join("fonts")
    }
    /// Where kuma puts the overlay folders it copies or scaffolds.
    pub fn overlays_dir(&self) -> PathBuf {
        self.project_root().join("overlays")
    }
    /// Every library's `presets/` plus the project's own, lowest precedence first.
    pub fn all_preset_dirs(&self) -> Vec<PathBuf> {
        self.library_dirs.iter().map(|d| d.join("presets")).chain([self.project_root().join("presets")]).collect()
    }
    /// Where installed design systems are looked for. Unlike blueprints and presets these
    /// are not read at runtime: one is **copied into the project** and the agent reads it
    /// there, so the project is the destination rather than the last search path entry.
    pub fn design_system_dirs(&self) -> Vec<PathBuf> {
        self.library_dirs.iter().map(|d| d.join("design-systems")).collect()
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

/// The user's asset library: `$XDG_DATA_HOME/kuma` (`~/.local/share/kuma`), holding
/// `blueprints/` and `presets/`. `KUMA_LIBRARY` (a `:`-separated list) adds more, in
/// increasing precedence.
pub fn default_library_dirs() -> Vec<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut dirs = vec![data.join("kuma")];
    if let Some(extra) = std::env::var_os("KUMA_LIBRARY") {
        dirs.extend(std::env::split_paths(&extra));
    }
    dirs
}

/// The libraries a daemon reads: `default_library_dirs`, then, when `KUMA_LIBRARY` is unset
/// and the user library holds no blueprints, the checkout's own `library/`, the starter set.
///
/// The desktop shell has always done this for the daemons it starts (`libraryFallback` in
/// `app/src/kuma.ts`, which also says so in the launcher). A daemon `kuma mcp` or `kuma
/// serve` started did not, so before `kuma library` had been run it had no blueprints and
/// none of the fonts they name: the same project drew its titles in fallback fonts or not,
/// depending on which process had started the daemon. `health` lists the folders either way.
pub fn library_dirs(repo_root: &Path) -> Vec<PathBuf> {
    with_starter_set(default_library_dirs(), std::env::var_os("KUMA_LIBRARY").is_some(), repo_root)
}

/// `dirs` (the user library first), and the checkout's `library/` after them when nothing
/// else was asked for and the user library has no blueprints.
fn with_starter_set(mut dirs: Vec<PathBuf>, asked: bool, repo_root: &Path) -> Vec<PathBuf> {
    let installed = std::fs::read_dir(dirs[0].join("blueprints")).is_ok_and(|mut d| d.next().is_some());
    let shipped = repo_root.join("library");
    if !asked && !installed && shipped.join("blueprints").is_dir() {
        dirs.push(shipped);
    }
    dirs
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
    /// The pin list, whole and as `GET /api/pins` lists it, sent whenever it can have
    /// changed: a marker edited (by an edit, a step or a restore), a pin answered, ticked or
    /// unticked. At most one per operation, after its `history`.
    Pins { pins: Vec<crate::pins::Pin> },
}

pub struct UndoStack {
    pub past: Vec<(Project, String)>,
    pub future: Vec<(Project, String)>,
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
    /// Which pins are done, by id. Session state like the history it is derived from.
    pins: Mutex<crate::pins::Answers>,
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
    /// The pins it answers.
    pub pins: &'a [String],
}

/// An edit's result as `AppState::edit` wants it from the closure that makes it: the
/// document, and what the history says it touched.
struct Edited {
    outcome: edit::EditOutcome,
    targets: Vec<Target>,
    ops: usize,
}

#[derive(Debug, Serialize)]
pub struct BlueprintInfo {
    pub id: String,
    pub manifest: OverlayManifest,
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
        // The shared `_vendor/` is the highest-precedence library that has one; project
        // overlays use it too.
        let vendor_dir = config.blueprint_dirs().iter().rev().map(|d| d.join("_vendor")).find(|d| d.is_dir()).unwrap_or_else(|| config.user_blueprint_dir().join("_vendor"));
        let baker = Baker::discover(&config.repo_root, vendor_dir, config.bakes_dir()).with_fonts(config.font_dirs());
        let transcriber = Transcriber::discover(&config.repo_root, config.library_dirs[0].join("models"));
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
            pins: Mutex::new(Default::default()),
            config,
        };
        // The revision it opened at can be restored like any it makes.
        lock(&st.history).keep(read(&st.project).clone());
        st.reload_blueprints();
        if !st.config.project_file.is_file() {
            st.save()?;
        }
        Ok(st)
    }

    /// Later libraries win on an id clash.
    pub fn reload_blueprints(&self) {
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
            .map(|(id, (_, m))| BlueprintInfo { id: id.clone(), manifest: m.clone() })
            .collect()
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

    /// Persist `next`, then make it current. Nothing changes in memory if the write
    /// fails, so a failed edit never leaves clients on a revision the disk lacks.
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
        // Validate on a copy: no user-facing code runs while the project lock is held.
        let current = read(&self.project).clone();
        let Edited { outcome, targets, ops } = f(&current)?;
        let rev = outcome.project.revision;
        let next = outcome.project.clone();
        let prev = self.commit(outcome.project, origin).map_err(|e| EditError::Patch(e.to_string()))?;
        let entry = history::Entry { ops, base_revision: step.base_revision, pins: step.pins.to_vec(), ..history::Entry::new(kind, origin, Some(rev), &targets) };
        self.landed(prev, next, &targets, entry, |u, prev| {
            u.past.push((prev, origin.to_string()));
            if u.past.len() > 200 {
                u.past.remove(0);
            }
            u.future.clear();
        });
        Ok((rev, outcome.warnings))
    }

    /// What every step does once `next` is committed over `prev`, in this order: diff the
    /// two, `push` `prev` onto the undo stack, record `entry` with the changes, send the
    /// pins if they can have changed, keep `next` to restore. The undo stack is not locked
    /// while the documents are diffed.
    fn landed(&self, prev: Project, next: Project, targets: &[Target], entry: history::Entry, push: impl FnOnce(&mut UndoStack, Project)) {
        let changes = history::changes(&prev, &next, targets);
        let pins_differ = crate::pins::differ(&prev, &next);
        push(&mut lock(&self.undo), prev);
        let answered = self.record(history::Entry { changes, ..entry });
        if answered || pins_differ {
            self.send_pins(&lock(&self.pins));
        }
        lock(&self.history).keep(next);
    }

    /// Append to the history and tell every client; an entry naming pins answers them.
    /// Returns whether it did, for the caller to send the pins once with whatever else
    /// changed them.
    fn record(&self, entry: history::Entry) -> bool {
        let answering = entry.revision.filter(|_| !entry.pins.is_empty());
        if let Some(revision) = answering {
            let mut answers = lock(&self.pins);
            for id in &entry.pins {
                answers.insert(id.clone(), crate::pins::Done { revision, by: entry.author.clone() });
            }
        }
        lock(&self.history).push(entry.clone());
        let _ = self.events.send(Event::History(entry));
        answering.is_some()
    }

    /// Every pin, the master's first, each with whether it is done: `GET /api/pins`.
    pub fn pin_list(&self) -> Vec<crate::pins::Pin> {
        let answers = lock(&self.pins);
        crate::pins::list(&read(&self.project), &answers)
    }

    /// Broadcast the pin list. Built and sent while `answers` (the pins lock) is held, so
    /// whichever list is sent last was built last and clients end on the current one. The
    /// pins lock is always taken before the project's.
    fn send_pins(&self, answers: &crate::pins::Answers) {
        let pins = crate::pins::list(&read(&self.project), answers);
        let _ = self.events.send(Event::Pins { pins });
    }

    /// A patch, naming the pins it answers (none: `&[]`). A refusal is recorded too: the
    /// person sees what was tried and why it did not land, not only what did.
    pub fn apply_patch(&self, patch: &[json_patch::PatchOperation], base: Option<u64>, origin: &str, pins: &[String]) -> Result<(u64, Vec<Issue>), EditError> {
        let step = Step { base_revision: base, pins };
        let ctx = self.validation_context();
        let applied = self.edit(origin, Kind::Edit, &step, |p| {
            let outcome = edit::apply_patch(p, base, patch, &ctx)?;
            Ok(Edited { targets: history::patch_targets(p, Some(&outcome.project), patch), ops: patch.len(), outcome })
        });
        if let Err(e) = &applied {
            let targets = history::patch_targets(&read(&self.project), None, patch);
            // Answers nothing: it made no revision.
            self.record(history::Entry { ops: patch.len(), base_revision: base, error: Some(history::refusal(e)), pins: pins.to_vec(), ..history::Entry::new(Kind::Refused, origin, None, &targets) });
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
            let outcome = edit::apply_patch(p, base, &ops, &self.validation_context())?;
            Ok(Edited { targets: history::patch_targets(p, Some(&outcome.project), &ops), ops: ops.len(), outcome })
        })?;
        Ok((carried.expect("build ran"), rev, warnings))
    }

    /// Make the document what it was at `revision`, as one new edit: validated, undoable,
    /// and at a new revision number, so no client mistakes it for the old one — the stance
    /// undo takes. `origin` says who asked (`ui`, `claude`).
    pub fn restore(&self, revision: u64, origin: Option<&str>) -> Result<(u64, Vec<Issue>), RestoreError> {
        let doc = {
            let h = lock(&self.history);
            let snap = h.snapshot(revision).ok_or(RestoreError::Unknown(revision))?;
            serde_json::to_value(snap).map_err(|e| EditError::Patch(e.to_string()))?
        };
        let label = format!("{}: restore r{revision}", origin.unwrap_or("other"));
        let ctx = self.validation_context();
        Ok(self.edit(&label, Kind::Restore, &Step::default(), |p| {
            // `replace` sets the revision past the current one, whatever the snapshot says.
            let outcome = edit::replace(p, None, doc, &ctx)?;
            Ok(Edited { targets: history::diff_targets(p, &outcome.project), ops: 1, outcome })
        })?)
    }

    /// Tick a pin by hand (the current revision, by the user), or untick it, which forgets
    /// an edit's answer too, and send the list. `false` when there is no such pin.
    pub fn tick_pin(&self, id: &str, done: bool) -> bool {
        let mut answers = lock(&self.pins);
        let revision = {
            let p = read(&self.project);
            if !crate::pins::exists(&p, id) {
                return false;
            }
            p.revision
        };
        if done {
            answers.insert(id.to_string(), crate::pins::Done { revision, by: "user".into() });
        } else {
            answers.remove(id);
        }
        self.send_pins(&answers);
        true
    }

    /// Undo/redo swap whole snapshots; the revision keeps increasing so clients never
    /// confuse an undone state with an older one. `Ok(None)` means nothing to undo/redo.
    /// `by` is the origin of the request (`ui`, `claude`), which the history names as the
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
        let Some((mut restored, origin)) = popped else { return Ok(None) };
        let keep = restored.clone();
        restored.revision = read(&self.project).revision + 1;
        let rev = restored.revision;
        let next = restored.clone();
        let verb = if redo { "redo" } else { "undo" };
        let label = format!("{verb}:{origin}");
        match self.commit(restored, &label) {
            Ok(now) => {
                let targets = history::diff_targets(&now, &next);
                let entry = history::Entry {
                    author: by.map(history::author).unwrap_or("other").to_string(),
                    summary: format!("{verb}: {}", history::summary(&origin)),
                    ..history::Entry::new(if redo { Kind::Redo } else { Kind::Undo }, &label, Some(rev), &targets)
                };
                self.landed(now, next, &targets, entry, |u, now| if redo { &mut u.past } else { &mut u.future }.push((now, origin)));
                Ok(Some(rev))
            }
            Err(e) => {
                // A failed save changed nothing; the history step must survive it.
                let mut u = lock(&self.undo);
                if redo { &mut u.future } else { &mut u.past }.push((keep, origin));
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path, libraries: Vec<PathBuf>) -> Config {
        Config { project_file: dir.join("p/t.kuma.json"), repo_root: dir.to_path_buf(), library_dirs: libraries, bind: "127.0.0.1:0".parse().unwrap() }
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
        let shipped = repo.join("library");
        let dirs = |repo: &Path, asked: bool| with_starter_set(vec![user.clone()], asked, repo);
        let (both, alone) = ([user.clone(), shipped], std::slice::from_ref(&user));
        assert_eq!(dirs(&repo, false), both, "nothing installed");
        assert_eq!(dirs(&repo, true), alone, "KUMA_LIBRARY names the libraries");
        std::fs::create_dir_all(user.join("blueprints")).unwrap();
        assert_eq!(dirs(&repo, false), both, "an empty folder is not a library");
        std::fs::create_dir_all(user.join("blueprints/mine")).unwrap();
        assert_eq!(dirs(&repo, false), alone, "the user's own library");
        assert_eq!(dirs(&tmp.path().join("elsewhere"), false), alone, "no checkout, nothing to fall back on");
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
