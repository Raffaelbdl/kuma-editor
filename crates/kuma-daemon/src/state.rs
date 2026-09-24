//! Project state, persistence, undo, blueprints and event broadcast.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};

use kuma_core::edit::{self, EditError};
use kuma_core::manifest::{ManifestError, OverlayManifest};
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
    /// Every library's `blueprints/`, lowest precedence first. Never a project's.
    pub fn blueprint_dirs(&self) -> Vec<PathBuf> {
        self.library_dirs.iter().map(|d| d.join("blueprints")).collect()
    }
    /// Where "Save as blueprint" writes: the user library, not a `KUMA_LIBRARY` entry.
    pub fn user_blueprint_dir(&self) -> PathBuf {
        self.library_dirs[0].join("blueprints")
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
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let target = dst.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
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

/// Events pushed to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The project changed; clients should refetch.
    ProjectChanged { revision: u64, origin: String },
    Job(crate::jobs::Job),
    /// Finished jobs were cleared from the table; clients drop them too.
    JobsCleared { ids: Vec<u64> },
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
}

#[derive(Debug, Serialize)]
pub struct BlueprintInfo {
    pub id: String,
    pub manifest: OverlayManifest,
    pub has_thumb: bool,
    /// An animated WebP of it beside the still, from the same job.
    pub has_preview: bool,
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
        let baker = Baker::discover(&config.repo_root, vendor_dir, config.cache_dir().join("bakes"));
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
            config,
        };
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
            .map(|(id, (dir, m))| BlueprintInfo {
                id: id.clone(),
                manifest: m.clone(),
                has_thumb: dir.join("thumb.png").is_file(),
                has_preview: dir.join("preview.webp").is_file(),
            })
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
        // Files are checked, so a missing overlay folder is an error now, not at render.
        ValidationContext { root: self.config.project_root(), check_files: true }
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

    /// Compose a patch *from the project as it is now* and apply it, both under the edit
    /// lock. For work that takes minutes before it knows what to write — a transcription —
    /// where deciding against a snapshot taken at the start would race every edit made
    /// meanwhile. `build` must stay cheap: it runs with the lock held. `base` is checked
    /// like any patch's.
    pub fn apply_built<T>(&self, origin: &str, base: Option<u64>, build: impl FnOnce(&Project) -> anyhow::Result<(Vec<json_patch::PatchOperation>, T)>) -> anyhow::Result<(T, u64, Vec<Issue>)> {
        let mut carried = None;
        let (rev, warnings) = self.edit(origin, |p| {
            let (ops, value) = build(p).map_err(|e| EditError::Patch(format!("{e:#}")))?;
            carried = Some(value);
            edit::apply_patch(p, base, &ops, &self.validation_context())
        })?;
        Ok((carried.expect("build ran"), rev, warnings))
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
