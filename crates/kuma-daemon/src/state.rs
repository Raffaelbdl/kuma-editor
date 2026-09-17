//! Project state, persistence, undo, templates and event broadcast.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};

use kuma_core::edit::{self, EditError};
use kuma_core::template::{TemplateError, TemplateManifest};
use kuma_core::{Issue, Project, ValidationContext};
use kuma_overlay::{Baker, Transcriber};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct Config {
    /// Project file (`*.kuma.json`). Its directory is the project root.
    pub project_file: PathBuf,
    /// Repository root: `node_modules/hyperframes`, `ui/dist`, `engine/build`.
    pub repo_root: PathBuf,
    /// Asset libraries (each holds `overlays/` and `presets/`), lowest precedence first;
    /// the project's own `overlays/` and `presets/` are always searched last (and win).
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
    /// Every library's `overlays/` plus the project's own, lowest precedence first.
    pub fn all_template_dirs(&self) -> Vec<PathBuf> {
        self.library_dirs.iter().map(|d| d.join("overlays")).chain([self.project_root().join("overlays")]).collect()
    }
    /// Every library's `presets/` plus the project's own, lowest precedence first.
    pub fn all_preset_dirs(&self) -> Vec<PathBuf> {
        self.library_dirs.iter().map(|d| d.join("presets")).chain([self.project_root().join("presets")]).collect()
    }
}

/// The user's asset library: `$XDG_DATA_HOME/kuma` (`~/.local/share/kuma`), holding
/// `overlays/` and `presets/`. `KUMA_LIBRARY` (a `:`-separated list) adds more, in
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

/// Events pushed to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The project changed; clients should refetch.
    ProjectChanged { revision: u64, origin: String },
    Job(crate::jobs::Job),
}

pub struct UndoStack {
    pub past: Vec<(Project, String)>,
    pub future: Vec<(Project, String)>,
}

pub struct Templates {
    pub loaded: BTreeMap<String, (PathBuf, TemplateManifest)>,
    /// Manifests that failed to load, so the UI and the agent see why a template is missing.
    pub errors: Vec<TemplateError>,
}

pub struct AppState {
    pub config: Config,
    pub project: RwLock<Project>,
    /// Held for the whole of every mutation (check → validate → swap → save) so two
    /// edits against the same base revision cannot both succeed.
    edit: Mutex<()>,
    pub undo: Mutex<UndoStack>,
    pub templates: RwLock<Templates>,
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
}

#[derive(Debug, Serialize)]
pub struct TemplateInfo {
    pub id: String,
    pub manifest: TemplateManifest,
    pub has_thumb: bool,
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
            serde_json::from_str::<Project>(&text)?
        } else {
            Project::new_default(Project::name_from_path(&config.project_file), 1920, 1080, kuma_core::Fps::default())
        };
        std::fs::create_dir_all(config.cache_dir())?;
        // The shared `_vendor/` is the highest-precedence library that has one.
        let vendor_dir = config.all_template_dirs().iter().rev().map(|d| d.join("_vendor")).find(|d| d.is_dir()).unwrap_or_else(|| config.library_dirs[0].join("overlays/_vendor"));
        let baker = Baker::discover(&config.repo_root, vendor_dir, config.cache_dir().join("bakes"));
        let transcriber = Transcriber::discover(&config.repo_root, config.library_dirs[0].join("models"));
        let (events, _) = broadcast::channel(256);
        let st = AppState {
            templates: RwLock::new(Templates { loaded: BTreeMap::new(), errors: Vec::new() }),
            project: RwLock::new(project),
            edit: Mutex::new(()),
            undo: Mutex::new(UndoStack { past: Vec::new(), future: Vec::new() }),
            baker,
            transcriber,
            events,
            jobs: crate::jobs::Jobs::default(),
            preview: crate::preview::PreviewHandle::default(),
            open_sequence: Mutex::new(None),
            config,
        };
        st.reload_templates();
        if !st.config.project_file.is_file() {
            st.save()?;
        }
        Ok(st)
    }

    /// Later directories win on an id clash (the project's own `overlays/` is last).
    pub fn reload_templates(&self) {
        let mut all = BTreeMap::new();
        let mut errors = Vec::new();
        for dir in self.config.all_template_dirs() {
            let (loaded, errs) = TemplateManifest::load_all(&dir);
            all.extend(loaded);
            errors.extend(errs);
        }
        for e in &errors {
            tracing::warn!("template skipped: {e}");
        }
        // Zero templates is a configuration problem that otherwise looks like health:
        // every overlay edit will now be refused, so say why once, loudly.
        if all.is_empty() {
            tracing::warn!(
                "no overlay templates loaded from {:?} — overlay clips will be refused; run `kuma library` to install the starter set",
                self.config.all_template_dirs()
            );
        }
        *write(&self.templates) = Templates { loaded: all, errors };
    }

    pub fn template_infos(&self) -> Vec<TemplateInfo> {
        read(&self.templates)
            .loaded
            .iter()
            .map(|(id, (dir, m))| TemplateInfo { id: id.clone(), manifest: m.clone(), has_thumb: dir.join("thumb.png").is_file() })
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

    pub fn validation_context(&self) -> ValidationContext {
        // `Some` even when empty: the daemon knows what is installed, so an unresolvable
        // overlay id is an error rather than something to find out about at render time.
        ValidationContext { root: self.config.project_root(), templates: Some(read(&self.templates).loaded.clone()), check_files: true }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = read(&self.project).clone();
        self.write_file(&p)
    }

    fn write_file(&self, p: &Project) -> anyhow::Result<()> {
        let tmp = self.config.project_file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(p)?)?;
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

    fn edit<F>(&self, origin: &str, f: F) -> Result<(u64, Vec<Issue>), EditError>
    where
        F: FnOnce(&Project) -> Result<edit::EditOutcome, EditError>,
    {
        let _guard = lock(&self.edit);
        // Validate on a copy: no user-facing code runs while the project lock is held.
        let current = read(&self.project).clone();
        let out = f(&current)?;
        let rev = out.project.revision;
        let prev = self.commit(out.project, origin).map_err(|e| EditError::Patch(e.to_string()))?;
        let mut u = lock(&self.undo);
        u.past.push((prev, origin.to_string()));
        if u.past.len() > 200 {
            u.past.remove(0);
        }
        u.future.clear();
        Ok((rev, out.warnings))
    }

    pub fn apply_patch(&self, patch: &[json_patch::PatchOperation], base: Option<u64>, origin: &str) -> Result<(u64, Vec<Issue>), EditError> {
        self.edit(origin, |p| edit::apply_patch(p, base, patch, &self.validation_context()))
    }

    /// Undo/redo swap whole snapshots; the revision keeps increasing so clients never
    /// confuse an undone state with an older one. `Ok(None)` means nothing to undo/redo.
    pub fn undo(&self) -> anyhow::Result<Option<u64>> {
        self.swap(false)
    }

    pub fn redo(&self) -> anyhow::Result<Option<u64>> {
        self.swap(true)
    }

    fn swap(&self, redo: bool) -> anyhow::Result<Option<u64>> {
        let _guard = lock(&self.edit);
        let mut u = lock(&self.undo);
        let Some((mut restored, origin)) = (if redo { u.future.pop() } else { u.past.pop() }) else { return Ok(None) };
        let keep = restored.clone();
        restored.revision = read(&self.project).revision + 1;
        let rev = restored.revision;
        let label = format!("{}:{origin}", if redo { "redo" } else { "undo" });
        match self.commit(restored, &label) {
            Ok(now) => {
                if redo { &mut u.past } else { &mut u.future }.push((now, origin));
                Ok(Some(rev))
            }
            Err(e) => {
                // A failed save changed nothing; the history step must survive it.
                if redo { &mut u.future } else { &mut u.past }.push((keep, origin));
                Err(e)
            }
        }
    }
}
