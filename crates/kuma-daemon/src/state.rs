//! Project state, persistence, undo, templates and event broadcast.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use kuma_core::edit::{self, EditError};
use kuma_core::template::TemplateManifest;
use kuma_core::{Issue, Project, ValidationContext};
use kuma_overlay::Baker;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct Config {
    /// Project file (`*.kuma.json`). Its directory is the project root.
    pub project_file: PathBuf,
    /// Repository root: `overlays/` library and `node_modules/hyperframes`.
    pub repo_root: PathBuf,
    /// Optional built UI directory to serve at `/`.
    pub ui_dir: Option<PathBuf>,
    pub bind: SocketAddr,
    /// Path to the C++ preview engine binary, if built.
    pub preview_engine: Option<PathBuf>,
}

impl Config {
    pub fn project_root(&self) -> PathBuf {
        self.project_file.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.project_root().join(".kuma")
    }
}

/// Events pushed to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The project changed; clients should refetch.
    ProjectChanged { revision: u64, origin: String },
    JobProgress { id: u64, kind: String, percent: u8, message: String },
    JobDone { id: u64, kind: String, ok: bool, message: String, result: serde_json::Value },
}

pub struct UndoStack {
    pub past: Vec<(Project, String)>,
    pub future: Vec<(Project, String)>,
}

pub struct AppState {
    pub config: Config,
    pub project: RwLock<Project>,
    pub undo: Mutex<UndoStack>,
    pub templates: RwLock<BTreeMap<String, (PathBuf, TemplateManifest)>>,
    pub baker: Baker,
    pub events: broadcast::Sender<Event>,
    pub jobs: crate::jobs::Jobs,
    pub preview: crate::preview::PreviewHandle,
}

#[derive(Debug, Serialize)]
pub struct TemplateInfo {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: TemplateManifest,
    pub has_thumb: bool,
}

impl AppState {
    pub fn load(config: Config) -> anyhow::Result<Self> {
        let project = if config.project_file.is_file() {
            let text = std::fs::read_to_string(&config.project_file)?;
            serde_json::from_str::<Project>(&text)?
        } else {
            let mut p = Project::default();
            p.name = config
                .project_file
                .file_stem()
                .map(|s| s.to_string_lossy().trim_end_matches(".kuma").to_string())
                .unwrap_or_default();
            p
        };
        std::fs::create_dir_all(config.cache_dir())?;
        let baker = Baker::discover(&config.repo_root, config.cache_dir().join("bakes"));
        let (events, _) = broadcast::channel(256);
        let st = AppState {
            templates: RwLock::new(BTreeMap::new()),
            project: RwLock::new(project),
            undo: Mutex::new(UndoStack { past: Vec::new(), future: Vec::new() }),
            baker,
            events,
            jobs: crate::jobs::Jobs::default(),
            preview: crate::preview::PreviewHandle::default(),
            config,
        };
        st.reload_templates();
        if !st.config.project_file.is_file() {
            st.save()?;
        }
        Ok(st)
    }

    /// Project-local `overlays/` wins over the library.
    pub fn reload_templates(&self) {
        let mut all = TemplateManifest::load_all(&self.config.repo_root.join("overlays"));
        for (k, v) in TemplateManifest::load_all(&self.config.project_root().join("overlays")) {
            all.insert(k, v);
        }
        *self.templates.write().unwrap() = all;
    }

    pub fn template_infos(&self) -> Vec<TemplateInfo> {
        self.templates
            .read()
            .unwrap()
            .iter()
            .map(|(id, (dir, m))| TemplateInfo { id: id.clone(), dir: dir.clone(), manifest: m.clone(), has_thumb: dir.join("thumb.png").is_file() })
            .collect()
    }

    pub fn validation_context(&self) -> ValidationContext {
        ValidationContext {
            root: self.config.project_root(),
            templates: self.templates.read().unwrap().clone(),
            check_files: true,
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = self.project.read().unwrap();
        let text = serde_json::to_string_pretty(&*p)?;
        let tmp = self.config.project_file.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.config.project_file)?;
        Ok(())
    }

    fn commit(&self, next: Project, origin: &str, warnings: Vec<Issue>) -> anyhow::Result<(u64, Vec<Issue>)> {
        let prev = {
            let mut p = self.project.write().unwrap();
            std::mem::replace(&mut *p, next)
        };
        {
            let mut u = self.undo.lock().unwrap();
            u.past.push((prev, origin.to_string()));
            if u.past.len() > 200 {
                u.past.remove(0);
            }
            u.future.clear();
        }
        self.save()?;
        let rev = self.project.read().unwrap().revision;
        let _ = self.events.send(Event::ProjectChanged { revision: rev, origin: origin.to_string() });
        Ok((rev, warnings))
    }

    pub fn apply_patch(&self, patch: &[json_patch::PatchOperation], base: Option<u64>, origin: &str) -> Result<(u64, Vec<Issue>), EditError> {
        let out = {
            let p = self.project.read().unwrap();
            edit::apply_patch(&p, base, patch, &self.validation_context())?
        };
        self.commit(out.project, origin, out.warnings).map_err(|e| EditError::Patch(e.to_string()))
    }

    pub fn replace_project(&self, doc: serde_json::Value, base: Option<u64>, origin: &str) -> Result<(u64, Vec<Issue>), EditError> {
        let out = {
            let p = self.project.read().unwrap();
            edit::replace(&p, base, doc, &self.validation_context())?
        };
        self.commit(out.project, origin, out.warnings).map_err(|e| EditError::Patch(e.to_string()))
    }

    /// Undo/redo swap whole snapshots; the revision keeps increasing so clients never
    /// confuse an undone state with an older one.
    pub fn undo(&self) -> Option<u64> {
        let (prev, origin) = self.undo.lock().unwrap().past.pop()?;
        let mut cur = self.project.write().unwrap();
        let rev = cur.revision + 1;
        let mut restored = prev;
        restored.revision = rev;
        let now = std::mem::replace(&mut *cur, restored);
        drop(cur);
        self.undo.lock().unwrap().future.push((now, origin.clone()));
        let _ = self.save();
        let _ = self.events.send(Event::ProjectChanged { revision: rev, origin: format!("undo:{origin}") });
        Some(rev)
    }

    pub fn redo(&self) -> Option<u64> {
        let (next, origin) = self.undo.lock().unwrap().future.pop()?;
        let mut cur = self.project.write().unwrap();
        let rev = cur.revision + 1;
        let mut restored = next;
        restored.revision = rev;
        let now = std::mem::replace(&mut *cur, restored);
        drop(cur);
        self.undo.lock().unwrap().past.push((now, origin.clone()));
        let _ = self.save();
        let _ = self.events.send(Event::ProjectChanged { revision: rev, origin: format!("redo:{origin}") });
        Some(rev)
    }

    pub fn history(&self) -> (Vec<String>, Vec<String>) {
        let u = self.undo.lock().unwrap();
        (u.past.iter().map(|(_, o)| o.clone()).collect(), u.future.iter().map(|(_, o)| o.clone()).collect())
    }
}
