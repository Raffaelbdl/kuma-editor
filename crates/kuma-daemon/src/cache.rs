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
use kuma_overlay::Baker;
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

/// The cache by part, and its total size in bytes. Built only by [`Usage::of`], so the
/// total cannot disagree with the parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub bakes: Part,
    pub samples: Part,
    pub thumbs: Part,
    pub bytes: u64,
}

impl Usage {
    fn of(bakes: Part, samples: Part, thumbs: Part) -> Self {
        Usage { bakes, samples, thumbs, bytes: bakes.bytes + samples.bytes + thumbs.bytes }
    }
}

/// What a prune deleted, and what is left.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Pruned {
    pub freed: Usage,
    pub kept: Usage,
}

/// How big the cache is. Reads only.
pub fn usage(state: &AppState) -> std::io::Result<Usage> {
    let c = &state.config;
    Ok(Usage::of(tally(&c.bakes_dir())?, tally(&c.samples_dir())?, tally(&c.thumbs_dir())?))
}

/// The bakes the project still wants: one per overlay clip in **every** sequence — not only
/// the master, since the preview bakes nested ones, and not only what the master reaches,
/// since `kuma bake` warms unplaced ones on purpose — plus the preview bake each overlay
/// entry's thumbnail is cut from. Computed, not remembered: the key is a pure function of
/// the folder, the vars and the clip, so no rendering is needed to know it. Each folder is
/// read once, however many clips use it.
pub fn live_bakes(state: &AppState, project: &Project) -> HashSet<String> {
    // A folder that does not resolve cannot bake, so there is nothing of it to keep.
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

/// Each overlay folder the bin uses, resolved, with its [`Baker::folder_digest`].
type Folders<'p> = HashMap<&'p str, (crate::overlays::Resolved, String)>;
type Vars = serde_json::Map<String, serde_json::Value>;

/// An overlay entry's folder (as [`Folders`] holds it) and its vars.
fn overlay<'f, 'i>(folders: &'f Folders, item: &'i kuma_core::BinItem) -> Option<(&'f (crate::overlays::Resolved, String), &'i Vars)> {
    match &item.source {
        BinSource::Overlay { overlay, vars } => Some((folders.get(overlay.as_str())?, vars)),
        _ => None,
    }
}

/// Delete what nothing wants: bakes whose key is not live, sampled frames older than
/// [`SAMPLE_GRACE`], and thumbnails of entries no longer in the bin. `all` empties bakes
/// and samples outright, which costs a re-bake on the next preview; it still keeps the
/// thumbnails of entries in the bin, because nothing regenerates those until asked.
///
/// Waits until nothing is using the cache — a render holds it for its whole encode — and
/// never touches anything still being written (`kuma_overlay::is_in_flight`).
pub fn prune(state: &AppState, all: bool, progress: &dyn Fn(u8, &str)) -> anyhow::Result<Pruned> {
    let freed = {
        let _alone = exclusive(state, progress);
        progress(10, "pruning");
        let project = read(&state.project).clone();
        let live = if all { HashSet::new() } else { live_bakes(state, &project) };
        let now = SystemTime::now();
        let old = |meta: &std::fs::Metadata| meta.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age >= SAMPLE_GRACE);
        let c = &state.config;
        Usage::of(
            sweep(&c.bakes_dir(), |name, _| !live.contains(name))?,
            sweep(&c.samples_dir(), |_, meta| all || old(meta))?,
            sweep(&c.thumbs_dir(), |name, _| !project.bin.contains_key(crate::Config::thumb_owner(name)))?,
        )
    };
    // Measured once the cache is free again: nothing below deletes, so nobody waits on it.
    Ok(Pruned { freed, kept: usage(state)? })
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
                if !said {
                    said = true;
                    progress(0, "waiting for renders and samples to finish");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Each entry directly under `dir`, with its name, metadata and size in bytes. A directory
/// that is not there has none.
fn entries(dir: &Path) -> std::io::Result<impl Iterator<Item = (std::fs::DirEntry, String, std::fs::Metadata, u64)>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => Some(rd),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    Ok(rd.into_iter().flatten().flatten().filter_map(|e| {
        let meta = e.metadata().ok()?;
        let bytes = size(&e.path(), &meta);
        let name = e.file_name().to_string_lossy().into_owned();
        Some((e, name, meta, bytes))
    }))
}

/// What is directly under `dir`.
fn tally(dir: &Path) -> std::io::Result<Part> {
    Ok(entries(dir)?.fold(Part::default(), |p, (.., bytes)| Part { entries: p.entries + 1, bytes: p.bytes + bytes }))
}

/// Delete each finished entry directly under `dir` that `doomed` names, and say what went.
/// Anything still being written is never offered to it.
fn sweep(dir: &Path, doomed: impl Fn(&str, &std::fs::Metadata) -> bool) -> std::io::Result<Part> {
    let mut gone = Part::default();
    for (entry, name, meta, bytes) in entries(dir)? {
        if kuma_overlay::is_in_flight(&name) || !doomed(&name, &meta) {
            continue;
        }
        let path = entry.path();
        match if meta.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) } {
            Ok(()) => gone = Part { entries: gone.entries + 1, bytes: gone.bytes + bytes },
            // Gone already: someone else's cleanup got there first.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(gone)
}

/// Bytes under `path`, not following links.
fn size(path: &Path, meta: &std::fs::Metadata) -> u64 {
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map(|rd| rd.flatten().filter_map(|e| Some(size(&e.path(), &e.metadata().ok()?))).sum()).unwrap_or(0)
}
