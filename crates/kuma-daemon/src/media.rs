//! Media files in the bin: adding them — one, a list, or what a folder holds, as one edit —
//! correcting what was probed when a file changes on disk, and saying which entries the
//! video does not use. Probing runs outside the edit lock; what to write is decided
//! against the project as it is when the patch lands.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use kuma_core::manifest::MANIFEST;
use kuma_core::{BinItem, BinSource, Issue, MediaKind, Project};
use kuma_overlay::Probe;
use serde::Serialize;
use serde_json::{json, Value};

use crate::jobs::{bin_op, bin_replace_op, resolve_path};
use crate::state::{read, AppState};

/// How to add what the paths name.
#[derive(Debug, Clone, Default)]
pub struct AddHow {
    /// The entry's id. Only for a call that adds exactly one file; otherwise each id is
    /// minted from its file's name.
    pub id: Option<String>,
    /// The entry every new one is listed under.
    pub parent: Option<String>,
    /// Add what a folder's subfolders hold too.
    pub recursive: bool,
}

/// What an add did: the entries it made, in the order of the paths and then of the
/// folders' names, and what a folder held that it passed over, with why.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Added {
    pub ids: Vec<String>,
    pub skipped: Vec<Skipped>,
    pub revision: u64,
    pub warnings: Vec<Issue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

/// A file to add, and whether it was named or found in a folder: a named file that cannot
/// be added fails the call, since it was asked for; one a folder held is skipped and said.
struct Candidate {
    abs: PathBuf,
    found: bool,
}

/// Probe files and add them to the bin in one edit, so a whole folder is one revision and
/// one undo. Each path is a file or a folder; a folder adds the media files directly in it
/// (by name, see [`kuma_overlay::media::MEDIA_EXTENSIONS`]), its subfolders' too with
/// `recursive`, and reports the rest as skipped. Hidden files and folders are passed over
/// without a word. A file a folder holds that is already in the bin is skipped, so adding
/// a folder again adds what is new in it; a file named outright is added however often.
/// One path naming an overlay folder (or its manifest) imports it, as it always has.
pub fn add(state: &AppState, paths: &[PathBuf], how: AddHow, base_revision: Option<u64>, origin: &str) -> anyhow::Result<Added> {
    anyhow::ensure!(!paths.is_empty(), "no path to add");
    let root = state.config.project_root();
    if let [path] = paths {
        let abs = resolve_path(state, path);
        if is_overlay(&abs) {
            let import = crate::overlays::Import { id: how.id, parent: how.parent, ..Default::default() };
            let (id, revision, warnings) = crate::overlays::import_overlay(state, &abs.to_string_lossy(), import, base_revision, origin)?;
            return Ok(Added { ids: vec![id], skipped: vec![], revision, warnings });
        }
    }

    let mut skipped = Vec::new();
    let mut candidates = Vec::new();
    for path in paths {
        let abs = resolve_path(state, path);
        let shown = shown(&root, &abs);
        if is_overlay(&abs) {
            anyhow::bail!("{shown} is an overlay folder: add it on its own, or with import_overlay");
        } else if abs.is_dir() {
            scan(&root, &abs, how.recursive, &mut candidates, &mut skipped)?;
        } else if !abs.is_file() {
            anyhow::bail!("file not found: {}", abs.display());
        } else if let Some(why) = kuma_core::unplayable_media(&abs.to_string_lossy()) {
            anyhow::bail!("{shown}: {why}");
        } else {
            candidates.push(Candidate { abs, found: false });
        }
    }
    if how.id.is_some() && candidates.len() != 1 {
        anyhow::bail!("`id` names one entry, and this adds {}: leave it out and each id is minted from its file's name", candidates.len());
    }

    let probes = probe_all(&candidates.iter().map(|c| c.abs.clone()).collect::<Vec<_>>());
    let mut probed = Vec::new();
    for (c, probe) in candidates.into_iter().zip(probes) {
        match probe {
            Ok(probe) => probed.push((c, probe)),
            Err(e) if c.found => skipped.push(Skipped { path: shown(&root, &c.abs), reason: format!("ffprobe cannot read it: {e}") }),
            Err(e) => anyhow::bail!("{}: {e}", shown(&root, &c.abs)),
        }
    }

    let ((ids, more), revision, warnings) = state.apply_built(origin, base_revision, |p| {
        let mut ops = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let mut more = Vec::new();
        for (c, probe) in &probed {
            let rel = c.abs.strip_prefix(&root).unwrap_or(&c.abs).to_string_lossy().into_owned();
            if c.found {
                if let Some(have) = entry_for(p, &rel) {
                    more.push(Skipped { path: rel, reason: format!("already in the bin as `{have}`") });
                    continue;
                }
            }
            let stem = c.abs.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("media".into());
            let id = match &how.id {
                // JSON Patch `add` would silently replace an existing member, rebinding every clip.
                Some(id) => {
                    anyhow::ensure!(!p.bin.contains_key(id), "bin id `{id}` already exists (pass a different id)");
                    id.clone()
                }
                None => {
                    let base = slug(&stem, probe.kind);
                    let free = |id: &String| !p.bin.contains_key(id) && !ids.contains(id);
                    std::iter::once(base.clone()).chain((2..).map(|n| format!("{base}-{n}"))).find(free).unwrap_or_default()
                }
            };
            let item = BinItem {
                name: stem,
                source: BinSource::File { path: rel, kind: probe.kind, proxy: None },
                meta: Some(probe.meta.clone()),
                parent: how.parent.clone(),
                order: None,
                volume: 1.0,
            };
            ops.push(bin_op(&id, "", serde_json::to_value(item)?)?);
            ids.push(id);
        }
        if ops.is_empty() {
            let all: Vec<String> = skipped.iter().chain(&more).map(|s| format!("{} ({})", s.path, s.reason)).collect();
            anyhow::bail!("no media to add{}", if all.is_empty() { String::new() } else { format!(": {}", all.join("; ")) });
        }
        Ok((ops, (ids, more)))
    })?;
    skipped.extend(more);
    Ok(Added { ids, skipped, revision, warnings })
}

/// A folder's own files, by name; its subfolders' with `recursive`.
fn scan(root: &Path, dir: &Path, recursive: bool, out: &mut Vec<Candidate>, skipped: &mut Vec<Skipped>) -> anyhow::Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for abs in entries {
        if abs.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        let skip = |reason: &str| Skipped { path: shown(root, &abs), reason: reason.to_string() };
        if abs.is_dir() {
            if abs.join(MANIFEST).is_file() {
                skipped.push(skip("an overlay folder: add it with import_overlay"));
            } else if recursive {
                scan(root, &abs, recursive, out, skipped)?;
            } else {
                skipped.push(skip("a folder: pass `recursive` to add what is in it"));
            }
        } else if let Some(why) = kuma_core::unplayable_media(&abs.to_string_lossy()) {
            skipped.push(skip(why));
        } else if !kuma_overlay::media::is_media_name(&abs) {
            skipped.push(skip("not named like a media file"));
        } else {
            out.push(Candidate { abs, found: true });
        }
    }
    Ok(())
}

fn is_overlay(abs: &Path) -> bool {
    abs.join(MANIFEST).is_file() || abs.file_name().is_some_and(|f| f == MANIFEST)
}

/// `abs` as the project would store it: relative when it is inside.
fn shown(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root).unwrap_or(abs).to_string_lossy().into_owned()
}

/// The file entry that already plays `rel`, if one does.
fn entry_for<'a>(p: &'a Project, rel: &str) -> Option<&'a String> {
    p.bin.iter().find(|(_, i)| matches!(&i.source, BinSource::File { path, .. } if path == rel)).map(|(id, _)| id)
}

/// A bin id from a file's stem: lowercase ASCII letters and digits, anything else `_` —
/// the id a single add has always minted. Ids must start with a letter or `_`, so a stem
/// that does not (`01 intro`) is prefixed with what the file is: `video_01_intro`.
fn slug(stem: &str, kind: MediaKind) -> String {
    let s: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    if kuma_core::is_valid_id(&s) {
        return s;
    }
    let kind = match kind {
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::Image => "image",
    };
    format!("{kind}_{s}")
}

/// Probe every file, a few at a time: ffprobe is a process per file, and a folder of sixty
/// one after another is seconds of waiting on nothing. Results are in `paths`' order.
fn probe_all(paths: &[PathBuf]) -> Vec<Result<Probe, String>> {
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let per = paths.len().div_ceil(workers).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = paths.chunks(per).map(|chunk| s.spawn(move || chunk.iter().map(|p| kuma_overlay::probe(p)).collect::<Vec<_>>())).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// Probe file entries again and write what changed: `bins`, or every file entry. For a
/// file replaced on disk, which the document cannot notice by itself (see "Stale probed
/// metadata" in `docs/decisions.md`): correcting it is asked for and is one revision-checked,
/// undoable patch that touches only the entries whose probe now says something else — their
/// `meta`, and their `kind` if the file is now another kind of thing. A kind change that
/// breaks a placement (an `in` on what is now a still) is refused by the validator like any
/// edit, and the whole correction with it. Nothing changed, nothing written.
///
/// The answer names, per entry, each field that changed with what it was and what it is.
pub fn reprobe(state: &AppState, bins: Option<&[String]>, base_revision: Option<u64>, origin: &str) -> anyhow::Result<Value> {
    let project = read(&state.project).clone();
    let targets: Vec<(String, String)> = match bins {
        Some(ids) => ids
            .iter()
            .map(|id| match project.bin.get(id).map(|i| &i.source) {
                Some(BinSource::File { path, .. }) => Ok((id.clone(), path.clone())),
                Some(_) => Err(anyhow::anyhow!("`{id}` is not a media file")),
                None => Err(anyhow::anyhow!("no bin entry `{id}`")),
            })
            .collect::<anyhow::Result<_>>()?,
        None => project.bin.iter().filter_map(|(id, i)| match &i.source {
            BinSource::File { path, .. } => Some((id.clone(), path.clone())),
            _ => None,
        }).collect(),
    };
    let probes = probe_all(&targets.iter().map(|(_, path)| resolve_path(state, Path::new(path))).collect::<Vec<_>>());
    let mut failed = Vec::new();
    let mut probed = Vec::new();
    for ((id, path), probe) in targets.into_iter().zip(probes) {
        match probe {
            Ok(probe) => probed.push((id, path, probe)),
            Err(reason) => failed.push(json!({ "id": id, "reason": reason })),
        }
    }

    // Looked at first against the snapshot, so a re-probe that finds nothing new writes no
    // revision; decided again under the lock, against the project as it is then.
    let differences = |p: &Project| -> anyhow::Result<(Vec<json_patch::PatchOperation>, Vec<Value>)> {
        let mut ops = Vec::new();
        let mut changed = Vec::new();
        for (id, path, probe) in &probed {
            let Some(item) = p.bin.get(id) else { continue };
            let BinSource::File { path: now, kind, .. } = &item.source else { continue };
            // Repointed while it was probed: what was probed is not this entry's file.
            if now != path {
                continue;
            }
            let mut changes = serde_json::Map::new();
            if *kind != probe.kind {
                changes.insert("kind".into(), json!({ "from": kind, "to": probe.kind }));
                ops.push(bin_replace_op(id, "/source/kind", serde_json::to_value(probe.kind)?)?);
            }
            let was = serde_json::to_value(&item.meta)?;
            let is = serde_json::to_value(&probe.meta)?;
            if was != is {
                let empty = serde_json::Map::new();
                let (a, b) = (was.as_object().unwrap_or(&empty), is.as_object().unwrap_or(&empty));
                let fields: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
                // A flag is written only when set, so one side without it is `false`.
                let side = |m: &serde_json::Map<String, Value>, other: &serde_json::Map<String, Value>, f: &String| {
                    m.get(f).cloned().unwrap_or_else(|| if other.get(f).is_some_and(Value::is_boolean) { Value::Bool(false) } else { Value::Null })
                };
                for f in fields {
                    let (from, to) = (side(a, b, f), side(b, a, f));
                    if from != to {
                        changes.insert(format!("meta.{f}"), json!({ "from": from, "to": to }));
                    }
                }
                ops.push(bin_replace_op(id, "/meta", is)?);
            }
            if !changes.is_empty() {
                changed.push(json!({ "id": id, "changes": changes }));
            }
        }
        Ok((ops, changed))
    };
    let unchanged = |changed: &[Value]| -> Vec<String> {
        let moved: HashSet<&str> = changed.iter().filter_map(|c| c["id"].as_str()).collect();
        probed.iter().map(|(id, _, _)| id.clone()).filter(|id| !moved.contains(id.as_str())).collect()
    };
    if differences(&project)?.0.is_empty() {
        let (revision, warnings) = (project.revision, state.validate(&project));
        return Ok(json!({ "changed": [], "unchanged": unchanged(&[]), "failed": failed, "revision": revision, "warnings": warnings }));
    }
    let (changed, revision, warnings) = state.apply_built(origin, base_revision, |p| differences(p))?;
    Ok(json!({ "changed": changed, "unchanged": unchanged(&changed), "failed": failed, "revision": revision, "warnings": warnings }))
}

/// [`Project::unused_entries`], each with its name and what it is, for a client to show.
pub fn unused(project: &Project) -> Value {
    let describe = |id: &str| {
        let item = &project.bin[id];
        let kind = match &item.source {
            BinSource::File { kind, .. } => serde_json::to_value(kind).unwrap_or(Value::Null),
            BinSource::Overlay { .. } => "overlay".into(),
            BinSource::Color { .. } => "color".into(),
            BinSource::Sequence(_) => "sequence".into(),
            BinSource::Folder { .. } => "folder".into(),
        };
        json!({ "id": id, "name": item.display_name(id), "kind": kind })
    };
    let found = project.unused_entries();
    let unreached: Vec<Value> = found
        .unreached
        .iter()
        .map(|(id, seqs)| {
            let mut v = describe(id);
            v["placed_in"] = json!(seqs);
            v
        })
        .collect();
    json!({ "unplaced": found.unplaced.iter().map(|id| describe(id)).collect::<Vec<_>>(), "unreached": unreached })
}
