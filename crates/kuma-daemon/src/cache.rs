//! The `.kuma/` cache: what it holds, and pruning what nothing wants any more.
//!
//! Three parts grow without bound and none of them deletes anything by itself: bakes
//! (content-hashed, so every edit to an overlay or its clip strands the previous one),
//! sampled frames (keyed by the compiled document, so every edit strands those too) and
//! bin thumbnails (keyed by entry, so a removed entry leaves its own behind). A prune is
//! asked for, never run behind anyone's back: a size or age cap could evict the bake a
//! render is about to read.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{RwLockWriteGuard, TryLockError};
use std::time::{Duration, SystemTime};

use kuma_core::{BinSource, Project};
use serde::{Deserialize, Serialize};

use crate::state::{read, AppState};

/// A sampled frame this recent survives a prune: its path may just have been handed to a
/// caller — an agent, the snapshot route — that has not read it yet.
pub const SAMPLE_GRACE: Duration = Duration::from_secs(60);

/// One part of the cache: how many entries (a bake is one directory) and their bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub entries: u64,
    pub bytes: u64,
}

impl Part {
    fn add(&mut self, bytes: u64) {
        self.entries += 1;
        self.bytes += bytes;
    }
}

/// The cache by part, and its total size in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub bakes: Part,
    pub samples: Part,
    pub thumbs: Part,
    pub bytes: u64,
}

impl Usage {
    fn total(mut self) -> Self {
        self.bytes = self.bakes.bytes + self.samples.bytes + self.thumbs.bytes;
        self
    }
}

/// What a prune deleted, and what is left.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Pruned {
    pub freed: Usage,
    pub kept: Usage,
}

/// How big the cache is. Reads only.
pub fn usage(state: &AppState) -> Usage {
    let dir = state.config.cache_dir();
    let tally = |sub: &str| {
        let mut part = Part::default();
        let _ = sweep(&dir.join(sub), &mut Part::default(), |_, _| false, &mut part);
        part
    };
    Usage { bakes: tally("bakes"), samples: tally("samples"), thumbs: tally("thumbs"), bytes: 0 }.total()
}

/// The bakes the project still wants: one per overlay clip in **every** sequence — not only
/// the master, since the preview bakes nested ones, and not only what the master reaches,
/// since `kuma bake` warms unplaced ones on purpose — plus the preview bake each overlay
/// entry's thumbnail is cut from. Computed, not remembered: the key is a pure function of
/// the folder, the vars and the clip, so no rendering is needed to know it.
pub fn live_bakes(state: &AppState, project: &Project) -> HashSet<String> {
    let mut folders: HashMap<String, Option<crate::overlays::Resolved>> = HashMap::new();
    let mut live = HashSet::new();
    // A folder that does not resolve cannot bake, so there is nothing of it to keep.
    let mut resolve = |rel: &str| folders.entry(rel.to_string()).or_insert_with(|| crate::overlays::resolve(state, rel).ok()).clone();
    for (_, _, clip) in project.all_clips() {
        let Some(BinSource::Overlay { overlay, vars }) = project.item(clip).map(|i| &i.source) else { continue };
        let Some(r) = resolve(overlay) else { continue };
        if let Ok(key) = state.baker.key(&crate::jobs::bake_request(&r, vars, project, clip)) {
            live.insert(key);
        }
    }
    for item in project.bin.values() {
        let BinSource::Overlay { overlay, vars } = &item.source else { continue };
        let Some(r) = resolve(overlay) else { continue };
        if let Ok(key) = state.baker.preview_key(&r.dir, &r.manifest, vars) {
            live.insert(key);
        }
    }
    live
}

/// Delete what nothing wants: bakes whose key is not live, sampled frames older than
/// [`SAMPLE_GRACE`], and thumbnails of entries no longer in the bin. `all` empties bakes
/// and samples outright, which costs a re-bake on the next preview; it still keeps the
/// thumbnails of entries in the bin, because nothing regenerates those until asked.
///
/// Waits until nothing is using the cache — a render holds it for its whole encode — and
/// never touches a bake or frame still being written.
pub fn prune(state: &AppState, all: bool, progress: &dyn Fn(u8, &str)) -> anyhow::Result<Pruned> {
    let _alone = exclusive(state, progress);
    progress(10, "pruning");
    let project = read(&state.project).clone();
    let live = if all { HashSet::new() } else { live_bakes(state, &project) };
    let dir = state.config.cache_dir();
    let now = SystemTime::now();
    let old = |meta: &std::fs::Metadata| meta.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age >= SAMPLE_GRACE);
    let (mut freed, mut kept) = (Usage::default(), Usage::default());
    sweep(&dir.join("bakes"), &mut freed.bakes, |name, _| !kuma_overlay::is_in_flight(name) && !live.contains(name), &mut kept.bakes)?;
    // `tempfile`'s own names, which a sample writes before its rename.
    sweep(&dir.join("samples"), &mut freed.samples, |name, meta| !name.starts_with(".tmp") && (all || old(meta)), &mut kept.samples)?;
    // `<id>.png`, `<id>.webp`, and `<id>.webp.tmp` while one encodes. Ids have no dots.
    let orphan = |name: &str| !name.ends_with(".tmp") && name.split('.').next().is_none_or(|id| !project.bin.contains_key(id));
    sweep(&dir.join("thumbs"), &mut freed.thumbs, |name, _| orphan(name), &mut kept.thumbs)?;
    Ok(Pruned { freed: freed.total(), kept: kept.total() })
}

/// Hold the cache alone. Polled rather than blocked on: a writer waiting on the lock would
/// hold up every sample and snapshot behind a render that may encode for an hour, and
/// the preview must not freeze because a prune was asked for.
fn exclusive<'a>(state: &'a AppState, progress: &dyn Fn(u8, &str)) -> RwLockWriteGuard<'a, ()> {
    let mut said = false;
    loop {
        match state.cache_users.try_write() {
            Ok(guard) => return guard,
            Err(TryLockError::Poisoned(p)) => return p.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if !std::mem::replace(&mut said, true) {
                    progress(0, "waiting for renders and samples to finish");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Each entry directly under `dir`, sized: into `gone` and deleted when `doomed` says so,
/// else into `kept`. A directory that is not there is an empty part.
fn sweep(dir: &Path, gone: &mut Part, doomed: impl Fn(&str, &std::fs::Metadata) -> bool, kept: &mut Part) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let bytes = size(&path, &meta);
        if !doomed(&name, &meta) {
            kept.add(bytes);
            continue;
        }
        let removed = if meta.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        match removed {
            Ok(()) => gone.add(bytes),
            // Gone already: someone else's cleanup got there first.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Bytes under `path`, not following links.
fn size(path: &Path, meta: &std::fs::Metadata) -> u64 {
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|rd| rd.flatten().filter_map(|e| std::fs::symlink_metadata(e.path()).ok().map(|m| size(&e.path(), &m))).sum())
        .unwrap_or(0)
}
