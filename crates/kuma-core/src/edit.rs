//! Editing the project as a document: RFC 6902 JSON Patch with revision checks.
//!
//! Every mutation — from the UI or from the agent — goes through [`apply_patch`]. The
//! result is re-parsed against the typed model (so unknown fields and bad shapes are
//! rejected) and revalidated; only then is the revision bumped.

use std::collections::{HashMap, HashSet};

use json_patch::jsonptr::Token;
use serde_json::Value;

use crate::model::{Marker, Project};
use crate::validate::{has_errors, validate, Issue, ValidationContext};

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("revision mismatch: project is at {current}, edit was based on {base}")]
    RevisionMismatch { current: u64, base: u64 },
    /// The patch could not be applied or the result is not a project document.
    #[error("patch failed: {0}")]
    Patch(String),
    #[error("validation failed")]
    Invalid(Vec<Issue>),
}

/// Outcome of a successful edit.
#[derive(Debug, Clone)]
pub struct EditOutcome {
    pub project: Project,
    /// Warnings that did not block the edit.
    pub warnings: Vec<Issue>,
}

/// Apply a JSON Patch to `current`. `base_revision`, when given, must equal the current
/// revision. On success the revision is incremented.
pub fn apply_patch(
    current: &Project,
    base_revision: Option<u64>,
    patch: &[json_patch::PatchOperation],
    ctx: &ValidationContext,
) -> Result<EditOutcome, EditError> {
    check_base(current, base_revision)?;
    let mut doc: Value = serde_json::to_value(current).map_err(|e| EditError::Patch(e.to_string()))?;
    json_patch::patch(&mut doc, patch).map_err(|e| EditError::Patch(e.to_string()))?;
    finish(current, doc, ctx)
}

fn check_base(current: &Project, base_revision: Option<u64>) -> Result<(), EditError> {
    match base_revision {
        Some(base) if base != current.revision => Err(EditError::RevisionMismatch { current: current.revision, base }),
        _ => Ok(()),
    }
}

/// Replace the whole document (undo/redo).
pub fn replace(
    current: &Project,
    base_revision: Option<u64>,
    doc: Value,
    ctx: &ValidationContext,
) -> Result<EditOutcome, EditError> {
    check_base(current, base_revision)?;
    finish(current, doc, ctx)
}

/// [`replace`] with a document already typed — one this editor kept, so there is no JSON
/// to parse it from. The revision is still the editor's, and it is validated all the same.
pub fn replace_project(
    current: &Project,
    base_revision: Option<u64>,
    mut project: Project,
    ctx: &ValidationContext,
) -> Result<EditOutcome, EditError> {
    check_base(current, base_revision)?;
    project.revision = current.revision + 1;
    checked(project, ctx)
}

fn finish(current: &Project, mut doc: Value, ctx: &ValidationContext) -> Result<EditOutcome, EditError> {
    // The revision is owned by the editor, never by the patch.
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("revision".into(), Value::from(current.revision + 1));
    }
    let project: Project = serde_json::from_value(doc).map_err(|e| EditError::Patch(format!("result is not a valid project: {e}")))?;
    checked(project, ctx)
}

fn checked(project: Project, ctx: &ValidationContext) -> Result<EditOutcome, EditError> {
    let issues = validate(&project, ctx);
    if has_errors(&issues) {
        return Err(EditError::Invalid(issues));
    }
    Ok(EditOutcome { project, warnings: issues })
}

/// What [`diff`] found: the patch, and what it leaves or removes that a caller should say.
#[derive(Debug, Default)]
pub struct Diff {
    pub patch: json_patch::Patch,
    /// The places the script and someone else both changed since the base, left as `live`
    /// has them: JSON Pointers into `live` (a clip or track by its index there; one `live` no
    /// longer has, by its id), in document order. Always empty without a base.
    pub kept: Vec<String>,
    /// Without a base, the markers `live` has and the document lacks, which the patch
    /// removes: often a person's comments. Always 0 with a base.
    pub lost_markers: usize,
    /// The bin entries the patch removes whole, in `live`'s order: the ones the document
    /// lacks, unless `keep` (or, with a base, the ones the script dropped since).
    pub removed: Vec<String>,
    /// The base for the next diff once this patch is applied: the script's document, except
    /// that each place in [`Diff::kept`] keeps the old base's value, so the conflict is
    /// reported again until the script or the person settles it — rather than vanishing, and
    /// a 0-op diff then claiming the two agree. As kuma stores a project; `wanted` itself
    /// without a base.
    pub next_base: Value,
}

/// The JSON Patch that makes `live` the project `wanted` describes: what a script that
/// builds a whole project sends, instead of working out the difference itself. Both are
/// compared as kuma stores them, so a field the script left at its default, or wrote as
/// kuma would not, is no difference. Two things a script does not know are `live`'s to
/// keep: the revision, and the probed `meta` of an entry the script gives none for. With
/// `keep`, bin entries `wanted` does not have stay too (someone else's, or not yet placed);
/// without it they are removed. Apply the patch at `live`'s revision; made again
/// afterwards, it is empty.
///
/// With `base` — the document the script wrote last time, the one `live` was made from —
/// the diff is three-way: it sends only what the script changed since then, so what a
/// person changed in the editor meanwhile (a fade, a mute, a marker, a clip they added)
/// stays. Where both changed the same value, `live`'s stays and [`Diff::kept`] says where.
/// Clips and tracks are matched by id, markers by time, so a clip moved to another track
/// or a marker added is one change, not an index shift.
pub fn diff(live: &Project, base: Option<&str>, wanted: &str, keep: bool) -> serde_json::Result<Diff> {
    let as_live = |text: &str| -> serde_json::Result<Project> {
        let mut p = crate::parse_project(text)?;
        p.revision = live.revision;
        for (id, theirs) in &live.bin {
            match p.bin.get_mut(id) {
                Some(mine) if mine.meta.is_none() => mine.meta = theirs.meta.clone(),
                None if keep => drop(p.bin.insert(id.clone(), theirs.clone())),
                _ => {}
            }
        }
        Ok(p)
    };
    let wanted = as_live(wanted)?;
    let theirs = serde_json::to_value(live)?;
    let mut out = Diff::default();
    let target = match base {
        None => {
            out.next_base = serde_json::to_value(&wanted)?;
            out.lost_markers = live
                .sequences()
                .map(|(id, seq)| {
                    let have: HashSet<String> = wanted.seq(id).into_iter().flat_map(|s| &s.markers).map(|m| Marker::time_key(m.t)).collect();
                    seq.markers.iter().filter(|m| !have.contains(&Marker::time_key(m.t))).count()
                })
                .sum();
            serde_json::to_value(&wanted)?
        }
        Some(base) => {
            let base = serde_json::to_value(as_live(base)?)?;
            let mine = serde_json::to_value(&wanted)?;
            out.next_base = rebase(Some(&base), Some(&theirs), Some(&mine), "").expect("the script's document is there");
            merge(Some(&base), Some(&theirs), Some(&mine), "", &mut out.kept).expect("the script's document is there")
        }
    };
    out.removed = live.bin.keys().filter(|id| target["bin"].get(id.as_str()).is_none()).cloned().collect();
    out.patch = json_patch::diff(&theirs, &target);
    Ok(out)
}

/// The three-way merge of one value, `None` being absent: the script's change where only it
/// changed, `live`'s where only someone else did, and `live`'s, noted in `kept`, where both
/// did differently. `at` is the value's pointer in `live`.
fn merge(base: Option<&Value>, live: Option<&Value>, mine: Option<&Value>, at: &str, kept: &mut Vec<String>) -> Option<Value> {
    if mine == base || mine == live {
        return live.cloned();
    }
    if live == base {
        return mine.cloned();
    }
    if let (Some(Value::Array(b)), Some(Value::Array(l)), Some(Value::Array(m))) = (base, live, mine) {
        if let (Some(bk), Some(lk), Some(mk)) = (keys(at, b), keys(at, l), keys(at, m)) {
            return Some(Value::Array(merge_keyed(at, (b, &bk), (l, &lk), (m, &mk), kept)));
        }
    }
    if let (Some(Value::Object(b)), Some(Value::Object(l)), Some(Value::Object(m))) = (base, live, mine) {
        let mut out = serde_json::Map::new();
        for k in l.keys().chain(m.keys().filter(|k| !l.contains_key(*k))) {
            let path = format!("{at}/{}", Token::new(k).encoded());
            if let Some(v) = merge(b.get(k), l.get(k), m.get(k), &path, kept) {
                out.insert(k.clone(), v);
            }
        }
        return Some(Value::Object(out));
    }
    kept.push(at.to_string());
    live.cloned()
}

/// What the base of one value becomes once [`merge`]'s result is applied: the script's value
/// where the merge took it or nobody disagreed, and the old base's where [`merge`] kept
/// `live`'s over a change of the script's, so the next merge meets the same conflict.
fn rebase(base: Option<&Value>, live: Option<&Value>, mine: Option<&Value>, at: &str) -> Option<Value> {
    if mine == base || mine == live || live == base {
        return mine.cloned();
    }
    if let (Some(Value::Array(b)), Some(Value::Array(l)), Some(Value::Array(m))) = (base, live, mine) {
        if let (Some(bk), Some(lk), Some(mk)) = (keys(at, b), keys(at, l), keys(at, m)) {
            let index = |keys: &[String]| -> HashMap<String, usize> { keys.iter().cloned().enumerate().map(|(i, k)| (k, i)).collect() };
            let (bi, li) = (index(&bk), index(&lk));
            let mine_keys: HashSet<&String> = mk.iter().collect();
            let mut out: Vec<Value> = m.iter().zip(&mk).filter_map(|(v, k)| rebase(bi.get(k).map(|&i| &b[i]), li.get(k).map(|&i| &l[i]), Some(v), at)).collect();
            // An element the script dropped and someone else changed stays in the base.
            out.extend(b.iter().zip(&bk).filter(|(_, k)| !mine_keys.contains(k)).filter_map(|(v, k)| rebase(Some(v), li.get(k).map(|&i| &l[i]), None, at)));
            return Some(Value::Array(out));
        }
    }
    if let (Some(Value::Object(b)), Some(Value::Object(l)), Some(Value::Object(m))) = (base, live, mine) {
        let mut out = serde_json::Map::new();
        for k in m.keys().chain(b.keys().filter(|k| !m.contains_key(*k))) {
            let path = format!("{at}/{}", Token::new(k).encoded());
            if let Some(v) = rebase(b.get(k), l.get(k), m.get(k), &path) {
                out.insert(k.clone(), v);
            }
        }
        return Some(Value::Object(out));
    }
    base.cloned()
}

/// What matches an element of an array across the three documents: a track's or a clip's
/// `id`, a marker's [`Marker::time_key`]. Anything else — effects, keyframes, transitions —
/// has no identity apart from its place, and merges whole.
fn key(at: &str, v: &Value) -> Option<String> {
    if at.ends_with("/markers") {
        return v.get("t")?.as_f64().map(Marker::time_key);
    }
    v.get("id")?.as_str().map(str::to_string)
}

/// The [`key`] of every element of `a`, or `None` if one has none or two share one.
fn keys(at: &str, a: &[Value]) -> Option<Vec<String>> {
    let keys: Vec<String> = a.iter().map(|v| key(at, v)).collect::<Option<_>>()?;
    (keys.iter().collect::<HashSet<_>>().len() == keys.len()).then_some(keys)
}

/// [`merge`] of arrays whose elements have [`keys`], each given with them: element by
/// element, in the script's order, with what someone else added put back after the element
/// it followed in `live`.
fn merge_keyed(at: &str, (b, bk): (&[Value], &[String]), (l, lk): (&[Value], &[String]), (m, mk): (&[Value], &[String]), kept: &mut Vec<String>) -> Vec<Value> {
    let index = |keys: &[String]| -> HashMap<String, usize> { keys.iter().cloned().enumerate().map(|(i, k)| (k, i)).collect() };
    let (bi, li, mi) = (index(bk), index(lk), index(mk));
    let mut out: Vec<(&str, Value)> = Vec::new();
    for (v, k) in m.iter().zip(mk) {
        let in_live = li.get(k).copied();
        let path = format!("{at}/{}", in_live.map_or_else(|| Token::new(k).encoded().to_string(), |i| i.to_string()));
        if let Some(v) = merge(bi.get(k).map(|&i| &b[i]), in_live.map(|i| &l[i]), Some(v), &path, kept) {
            out.push((k, v));
        }
    }
    // What `live` has and the script does not: someone else's addition stays, an element the
    // script removed goes unless someone else changed it.
    for (i, (v, k)) in l.iter().zip(lk).enumerate() {
        if mi.contains_key(k) {
            continue;
        }
        let Some(v) = merge(bi.get(k).map(|&j| &b[j]), Some(v), None, &format!("{at}/{i}"), kept) else { continue };
        let after = lk[..i].iter().rev().find_map(|p| out.iter().position(|(k, _)| k == p));
        out.insert(after.map_or(0, |j| j + 1), (k, v));
    }
    out.into_iter().map(|(_, v)| v).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A script's document, written its own way, against the stored one: only what really
    /// differs is sent, and sending it leaves nothing to send.
    #[test]
    fn a_diff_sends_what_differs_and_then_nothing() {
        let live: Project = serde_json::from_value(json!({
            "revision": 7,
            "bin": {
                "a": { "source": { "path": "a.mp4" }, "meta": { "duration": 9.5, "has_audio": true } },
                "theirs": { "source": { "color": "#ffffff" } },
                "main": { "source": { "tracks": [{ "id": "v1", "clips": [{ "id": "c1", "start": 0, "end": 2, "bin": "a" }] }] } }
            }
        }))
        .unwrap();
        // No revision, no meta, defaults spelled out or left out as the script pleases.
        let wanted = |end: f64| json!({ "bin": {
            "a": { "source": { "path": "a.mp4" } },
            "main": { "source": { "tracks": [{ "id": "v1", "muted": false, "clips": [{ "id": "c1", "start": 0.0, "end": end, "bin": "a", "speed": 1 }] }] } }
        } }).to_string();
        assert_eq!(diff(&live, None, &wanted(2.0), true).unwrap().patch.0, [], "the same project, said differently");

        let ops = |keep| serde_json::to_value(diff(&live, None, &wanted(3.0), keep).unwrap().patch).unwrap();
        let retime = json!({ "op": "replace", "path": "/bin/main/source/tracks/0/clips/0/end", "value": 3.0 });
        assert_eq!(ops(true), json!([retime]));
        assert_eq!(ops(false), json!([retime, { "op": "remove", "path": "/bin/theirs" }]), "an entry the script does not make goes, unless kept");
        assert_eq!(diff(&live, None, &wanted(3.0), false).unwrap().removed, ["theirs"], "and the diff names it");
        assert!(diff(&live, None, &wanted(3.0), true).unwrap().removed.is_empty());

        let after = apply_patch(&live, Some(7), &diff(&live, None, &wanted(3.0), false).unwrap().patch.0, &ValidationContext::default()).unwrap().project;
        assert_eq!(after.bin["a"].meta, live.bin["a"].meta, "what was probed is kept");
        assert_eq!(diff(&after, None, &wanted(3.0), false).unwrap().patch.0, [], "applied, there is nothing left to send");
    }

    /// With the script's previous document as the base, the diff sends what the script
    /// changed and leaves what a person changed in the editor since: the case where a
    /// rebuild used to put every hand-made fade, mute and marker back as the script had it.
    #[test]
    fn a_three_way_diff_keeps_what_someone_else_changed() {
        let clip = |id: &str, start: f64, end: f64| json!({ "id": id, "start": start, "end": end, "bin": "a" });
        let doc = |clips: Vec<Value>, muted: bool, markers: Value| json!({ "bin": {
            "a": { "source": { "path": "a.mp4" } },
            "main": { "source": { "tracks": [
                { "id": "v1", "clips": clips },
                { "id": "v2", "muted": muted, "clips": [] }
            ], "markers": markers } }
        } });
        let base = doc(vec![clip("c1", 0.0, 2.0), clip("c2", 2.0, 4.0), clip("c3", 4.0, 6.0)], false, json!([]));
        // In the editor: a gain on c1, c3 deleted, a clip and a marker added, v2 muted.
        let mut person = doc(vec![json!({ "id": "c1", "start": 0, "end": 2, "bin": "a", "volume": 0.5 }), clip("c2", 2.0, 4.0), clip("mine", 6.0, 8.0)], true, json!([{ "t": 3.27, "label": "too loud" }]));
        person["revision"] = json!(12);
        let live: Project = serde_json::from_value(person).unwrap();
        // The script, run again: c2 shortened, c4 added, c3 still there as it was.
        let wanted = doc(vec![clip("c1", 0.0, 2.0), clip("c2", 2.0, 3.5), clip("c3", 4.0, 6.0), clip("c4", 8.0, 9.0)], false, json!([]));

        assert_eq!(diff(&live, None, &wanted.to_string(), false).unwrap().lost_markers, 1, "without a base the marker would go, and the diff says so");
        let out = diff(&live, Some(&base.to_string()), &wanted.to_string(), false).unwrap();
        assert_eq!((out.kept.len(), out.lost_markers), (0, 0));
        let after = apply_patch(&live, Some(12), &out.patch.0, &ValidationContext::default()).unwrap().project;
        let seq = after.seq("main").unwrap();
        let v1: Vec<(&str, f64, f64)> = seq.tracks[0].clips.iter().map(|c| (c.id.as_str(), c.end, c.volume)).collect();
        assert_eq!(v1, [("c1", 2.0, 0.5), ("c2", 3.5, 1.0), ("mine", 8.0, 1.0), ("c4", 9.0, 1.0)], "the script's changes and the person's, c3 left deleted");
        assert!(seq.tracks[1].muted, "the mute stays");
        assert_eq!(seq.markers.len(), 1, "the marker stays");
        assert_eq!(diff(&after, Some(&wanted.to_string()), &wanted.to_string(), false).unwrap().patch.0, [], "and with the new base there is nothing left to send");

        // Both changed c1's end: the person's stays, and the diff says where.
        let wanted = doc(vec![clip("c1", 0.0, 1.0), clip("c2", 2.0, 4.0), clip("c3", 4.0, 6.0)], false, json!([{ "t": 1.0, "label": "agent's" }]));
        let mut person = serde_json::to_value(&live).unwrap();
        person["bin"]["main"]["source"]["tracks"][0]["clips"][0]["end"] = json!(1.5);
        let person: Project = serde_json::from_value(person).unwrap();
        let out = diff(&person, Some(&base.to_string()), &wanted.to_string(), false).unwrap();
        assert_eq!(out.kept, ["/bin/main/source/tracks/0/clips/0/end"]);
        // Applied, with the next base this diff gives, the conflict is met again: the
        // script's 1.0 has not landed, so the two do not agree and the diff must say so.
        let next = out.next_base.to_string();
        let applied = apply_patch(&person, Some(12), &out.patch.0, &ValidationContext::default()).unwrap().project;
        let again = diff(&applied, Some(&next), &wanted.to_string(), false).unwrap();
        assert_eq!((again.patch.0.len(), again.kept.clone()), (0, vec!["/bin/main/source/tracks/0/clips/0/end".to_string()]), "the conflict is still reported");
        // Once the script takes the person's value, it is settled and nothing is left.
        let mut settled = wanted.clone();
        settled["bin"]["main"]["source"]["tracks"][0]["clips"][0]["end"] = json!(1.5);
        let done = diff(&applied, Some(&next), &settled.to_string(), false).unwrap();
        assert_eq!((done.patch.0.len(), done.kept.len()), (0, 0));
        let after = apply_patch(&person, Some(12), &out.patch.0, &ValidationContext::default()).unwrap().project;
        let seq = after.seq("main").unwrap();
        assert_eq!(seq.tracks[0].clips[0].end, 1.5);
        assert_eq!(seq.markers.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(), ["too loud", "agent's"], "markers are matched by time: both stay");
    }

    #[test]
    fn patch_bumps_revision_and_validates() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "a": { "source": { "path": "a.mp4" } },
                "main": { "source": { "tracks": [{ "id": "v1", "clips": [] }] } }
            }
        }))
        .unwrap();
        let clips = "/bin/main/source/tracks/0/clips";
        let patch: json_patch::Patch = serde_json::from_value(json!([
            { "op": "add", "path": format!("{clips}/-"), "value": { "id": "c1", "start": 0, "end": 2, "bin": "a" } }
        ]))
        .unwrap();
        let out = apply_patch(&p, Some(0), &patch.0, &ValidationContext::default()).unwrap();
        assert_eq!(out.project.revision, 1);
        assert_eq!(out.project.master_seq().unwrap().tracks[0].clips.len(), 1);

        // Stale base revision is rejected.
        let err = apply_patch(&out.project, Some(0), &patch.0, &ValidationContext::default()).unwrap_err();
        assert!(matches!(err, EditError::RevisionMismatch { .. }));

        // A patch that produces an invalid project is rejected atomically.
        let bad: json_patch::Patch = serde_json::from_value(json!([
            { "op": "replace", "path": format!("{clips}/0/end"), "value": -1 }
        ]))
        .unwrap();
        let err = apply_patch(&out.project, None, &bad.0, &ValidationContext::default()).unwrap_err();
        assert!(matches!(err, EditError::Invalid(_)));

        // A typed replace takes the revision past the current one, whatever the document
        // says, and is validated like a patch.
        let back = replace_project(&out.project, Some(1), Project { revision: 0, ..p.clone() }, &ValidationContext::default()).unwrap();
        assert_eq!((back.project.revision, back.project.master_seq().unwrap().tracks[0].clips.len()), (2, 0));
        let mut broken = out.project.clone();
        broken.master = "nowhere".into();
        assert!(matches!(replace_project(&out.project, None, broken, &ValidationContext::default()), Err(EditError::Invalid(_))));
        assert!(matches!(replace_project(&out.project, Some(0), p, &ValidationContext::default()), Err(EditError::RevisionMismatch { .. })));
    }
}
