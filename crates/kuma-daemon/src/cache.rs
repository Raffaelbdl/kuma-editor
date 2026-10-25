//! The `.kuma/` cache: its size, and a prune of what the project no longer uses. Pruning is
//! asked for, never automatic: a cap could evict the bake a render is about to read.

use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::TryLockError;
use std::time::{Duration, SystemTime};

use kuma_core::{BinItem, BinSource, Effect, Project};
use kuma_overlay::Baker;
use serde::{Deserialize, Serialize};

use crate::overlays::Resolved;
use crate::state::{read, AppState};

/// A sampled frame younger than this survives a prune: a caller may not have read it yet.
pub const SAMPLE_GRACE: Duration = Duration::from_secs(60);

/// Entries (a bake is one directory) and their bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub entries: u64,
    pub bytes: u64,
}

impl Part {
    fn add(self, bytes: u64) -> Self {
        Part { entries: self.entries + 1, bytes: self.bytes + bytes }
    }
}

/// The cache by part. Built by [`Usage::of`], so `bytes` is always their sum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub bakes: Part,
    pub samples: Part,
    pub thumbs: Part,
    /// Grades compiled to LUT files, one per distinct grade.
    pub luts: Part,
    pub bytes: u64,
}

impl Usage {
    fn of(bakes: Part, samples: Part, thumbs: Part, luts: Part) -> Self {
        Usage { bakes, samples, thumbs, luts, bytes: bakes.bytes + samples.bytes + thumbs.bytes + luts.bytes }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Pruned {
    pub freed: Usage,
    pub kept: Usage,
}

pub fn usage(state: &AppState) -> std::io::Result<Usage> {
    let c = &state.config;
    Ok(Usage::of(tally(&c.bakes_dir())?, tally(&c.samples_dir())?, tally(&c.thumbs_dir())?, tally(&c.luts_dir())?))
}

/// Names of the LUT files the project's grades compile to. A grade whose own LUT will not
/// read has none, and cannot compile until it does, which writes it again.
pub fn live_luts(state: &AppState, project: &Project) -> HashSet<String> {
    let root = state.config.project_root();
    // Each distinct grade once: clips graded alike share a file. A grade is floats, so
    // this is a list, not a set.
    let mut seen = Vec::new();
    for g in project.all_clips().flat_map(|(_, _, c)| c.effects.iter()).filter_map(Effect::as_grade) {
        if !seen.contains(&g) {
            seen.push(g);
        }
    }
    seen.into_iter().filter_map(|g| kuma_mlt::luts::name(g, &root).ok().flatten()).collect()
}

/// Keys of the bakes the project still wants: every overlay clip in every sequence (the
/// preview bakes nested ones, `kuma bake` warms unplaced ones), plus each overlay entry's
/// thumbnail bake. Recomputed, since a key is a pure function of folder, vars and clip.
pub fn live_bakes(state: &AppState, project: &Project) -> HashSet<String> {
    // Hash each folder once however many clips use it; one that does not resolve cannot bake.
    let folders: Folders = project
        .overlay_folders()
        .into_iter()
        .filter_map(|rel| {
            let r = crate::overlays::resolve(state, rel).ok()?;
            let digest = state.baker.folder_digest(&r.dir).ok()?;
            Some((rel, (r, digest)))
        })
        .collect();
    let clips = crate::jobs::every_overlay_clip(project)
        .filter_map(|clip| overlay(&folders, project.item(clip)?).map(|((r, digest), vars)| (digest, crate::jobs::bake_request(r, vars, project, clip))));
    let previews = project.bin.values().filter_map(|item| overlay(&folders, item).map(|((r, digest), vars)| (digest, Baker::preview_request(&r.dir, &r.manifest, vars))));
    clips.chain(previews).filter_map(|(digest, req)| state.baker.key_with(digest, &req).ok()).collect()
}

type Folders<'p> = HashMap<&'p str, (Resolved, String)>;
type Vars = serde_json::Map<String, serde_json::Value>;

fn overlay<'f, 'i>(folders: &'f Folders, item: &'i BinItem) -> Option<(&'f (Resolved, String), &'i Vars)> {
    match &item.source {
        BinSource::Overlay { overlay, vars } => Some((folders.get(overlay.as_str())?, vars)),
        _ => None,
    }
}

/// Delete bakes whose key is not live, sampled frames older than [`SAMPLE_GRACE`],
/// thumbnails of entries gone from the bin and LUTs no grade compiles to. `all` deletes
/// every bake and frame too, but keeps the bin's thumbnails, which nothing regenerates
/// until asked, and the live LUTs, which the preview engine may have loaded. Waits for whatever
/// holds `cache_users`, and never touches scratch still being written (`is_in_flight`).
pub fn prune(state: &AppState, all: bool, progress: &dyn Fn(u8, &str)) -> anyhow::Result<Pruned> {
    let freed = {
        let _alone = exclusive(state, progress);
        progress(10, "pruning");
        let project = read(&state.project).clone();
        let live = if all { HashSet::new() } else { live_bakes(state, &project) };
        let luts = live_luts(state, &project);
        let now = SystemTime::now();
        let old = |meta: &Metadata| meta.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age >= SAMPLE_GRACE);
        let c = &state.config;
        Usage::of(
            sweep(&c.bakes_dir(), |name, _| !live.contains(name))?,
            sweep(&c.samples_dir(), |_, meta| all || old(meta))?,
            sweep(&c.thumbs_dir(), |name, _| !project.bin.contains_key(crate::Config::thumb_owner(name)))?,
            sweep(&c.luts_dir(), |name, _| !luts.contains(name))?,
        )
    };
    Ok(Pruned { freed, kept: usage(state)? })
}

/// Hold the cache alone. Polled: a writer blocked on the lock would stall every sample
/// behind a render that may encode for an hour.
fn exclusive<'a>(state: &'a AppState, progress: &dyn Fn(u8, &str)) -> std::sync::RwLockWriteGuard<'a, ()> {
    let mut said = false;
    loop {
        match state.cache_users.try_write() {
            Ok(guard) => return guard,
            Err(TryLockError::Poisoned(p)) => return p.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if !said {
                    said = true;
                    progress(0, "waiting for renders and samples to finish");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Each entry directly under `dir` (none if it is missing): path, name, metadata, bytes.
fn entries(dir: &Path) -> std::io::Result<impl Iterator<Item = (PathBuf, String, Metadata, u64)>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => Some(rd),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    Ok(rd.into_iter().flatten().flatten().filter_map(|e| {
        let meta = e.metadata().ok()?;
        let path = e.path();
        let bytes = size(&path, &meta);
        Some((path, e.file_name().to_string_lossy().into_owned(), meta, bytes))
    }))
}

fn tally(dir: &Path) -> std::io::Result<Part> {
    Ok(entries(dir)?.fold(Part::default(), |p, (.., bytes)| p.add(bytes)))
}

/// Delete the finished entries under `dir` that `doomed` names; return what went.
fn sweep(dir: &Path, doomed: impl Fn(&str, &Metadata) -> bool) -> std::io::Result<Part> {
    let mut gone = Part::default();
    for (path, name, meta, bytes) in entries(dir)? {
        if kuma_overlay::is_in_flight(&name) || !doomed(&name, &meta) {
            continue;
        }
        match if meta.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) } {
            Ok(()) => gone = gone.add(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(gone)
}

/// Bytes under `path`, not following links.
fn size(path: &Path, meta: &Metadata) -> u64 {
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map(|rd| rd.flatten().filter_map(|e| Some(size(&e.path(), &e.metadata().ok()?))).sum()).unwrap_or(0)
}
