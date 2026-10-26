//! Markers: notes on moments of a sequence — the person's, for the agent to answer, and
//! the agent's own.
//!
//! A marker is the document's (`Marker { t, label, color, origin }` in a sequence's
//! `markers`), and it says who left it: [`stamp`] writes the head of the adding edit's
//! origin into it as the edit lands. Seen from here it is listed with an id an edit can
//! name, who left it, and whether one has answered it. Markers have no id of their own, so
//! the id is derived from where the marker is (`<sequence>@<t>`), and whether it is done is
//! the daemon's, never written into the document: kept in a file beside it ([`save`],
//! [`load`]) so a restart does not reopen what was answered.

use std::collections::BTreeMap;
use std::path::Path;

use kuma_core::{BinId, BinSource, Marker, Project, Sequence};
use serde::{Deserialize, Serialize};

use crate::history;

/// A marker's id: its sequence and its time to the millisecond, `master@8.600`.
pub fn marker_id(sequence: &str, t: f64) -> String {
    format!("{sequence}@{t:.3}")
}

/// `agent`, `user` or `other`: who left `m`, read off its origin as an edit's author is. A
/// marker without one is an older file's, from before markers said, and those were all a
/// person's.
pub fn author(m: &Marker) -> &'static str {
    m.origin.as_deref().map_or("user", history::author)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkerInfo {
    pub id: String,
    pub sequence: BinId,
    /// 1-based, among all this sequence's markers in time order: what the editor numbers it.
    pub index: usize,
    pub t: f64,
    pub label: String,
    pub color: Option<String>,
    /// As the document has it: who left it (`ui`, `mcp`…), or null for an older marker.
    pub origin: Option<String>,
    /// `user`, `agent` or `other` ([`author`]).
    pub author: String,
    /// Left for the agent and not answered: a comment in its label, not the agent's own,
    /// not done. What the editor counts and the agent reads as a request.
    pub waiting: bool,
    /// The revision that answered it, or the one current when it was ticked by hand.
    pub done: Option<u64>,
    /// `agent`, `user` or `other`: who answered it (a tick is the user's).
    pub done_by: Option<String>,
}

/// How a marker was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub revision: u64,
    pub by: String,
}

/// Which markers are done, by id. The latest answer wins: an edit naming a marker, a tick,
/// or an untick, which forgets both. In id order, so the file [`save`] writes is the same for
/// the same answers.
pub type Answers = BTreeMap<String, Done>;

/// The answers kept in `path` for the markers `p` has: none when there is no file, or one
/// that does not read (said, not fatal: it costs the ticks, never the project). An answer
/// whose marker is gone — deleted, or moved, which changes its id — is dropped.
pub fn load(path: &Path, p: &Project) -> Answers {
    if !path.exists() {
        return Answers::new();
    }
    let read = || anyhow::Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?);
    let mut answers = read().unwrap_or_else(|e| {
        tracing::warn!("markers' answers not read from {}: {e:#}", path.display());
        Answers::new()
    });
    forget_gone(&mut answers, p);
    answers
}

/// Write `answers` to `path`, replacing it atomically.
pub fn save(path: &Path, answers: &Answers) -> anyhow::Result<()> {
    crate::jobs::write_atomic(path, &serde_json::to_vec_pretty(answers)?)
}

/// Drop the answers whose marker `p` no longer has — deleted, or moved, which changes its
/// id — so a marker put later at that moment starts unanswered. Whether it dropped any.
pub fn forget_gone(answers: &mut Answers, p: &Project) -> bool {
    let before = answers.len();
    answers.retain(|id, _| exists(p, id));
    answers.len() != before
}

/// Every marker in `p`: the master's first, then the other sequences' in bin order.
pub fn list(p: &Project, answers: &Answers) -> Vec<MarkerInfo> {
    let master = p.master_seq().map(|s| (&p.master, s));
    let others = p.sequences().filter(|(id, _)| **id != p.master);
    master.into_iter().chain(others).flat_map(|(id, s)| of_sequence(id, s, answers)).collect()
}

/// Whether `id` names a marker in `p`, found without listing them.
pub fn exists(p: &Project, id: &str) -> bool {
    let Some((seq, _)) = id.rsplit_once('@') else { return false };
    p.seq(seq).is_some_and(|s| s.markers.iter().any(|m| marker_id(seq, m.t) == id))
}

/// Whether `a` and `b` list different markers, done or not: one added, removed, moved,
/// relabelled, recoloured or attributed, or another master to list first. Compares the
/// markers in place rather than building either list.
pub fn differ(a: &Project, b: &Project) -> bool {
    fn marks(p: &Project) -> impl Iterator<Item = (&BinId, &Marker)> {
        p.sequences().flat_map(|(id, s)| s.markers.iter().map(move |m| (id, m)))
    }
    a.master != b.master || !marks(a).eq(marks(b))
}

/// Say who left each marker of `after` that does not say: the marker at the same time of
/// the same sequence in `before`, if there was one — so a client that writes a whole
/// `markers` array back without origins re-attributes nothing, and an older marker stays
/// unattributed — or else `origin`'s head, the edit that added it (`ui` when it has none,
/// the editor's default). A marker that says already keeps what it says.
pub fn stamp(before: &Project, after: &mut Project, origin: &str) {
    let head = match history::head(origin) {
        "" => "ui",
        h => h,
    };
    for (id, item) in after.bin.iter_mut() {
        let BinSource::Sequence(s) = &mut item.source else { continue };
        let was = before.seq(id).map_or(&[][..], |w| &w.markers[..]);
        for m in s.markers.iter_mut().filter(|m| m.origin.is_none()) {
            let mid = marker_id(id, m.t);
            let mut same = was.iter().filter(|o| marker_id(id, o.t) == mid).peekable();
            m.origin = match same.peek() {
                None => Some(head.to_string()),
                Some(_) => same.find_map(|o| o.origin.clone()),
            };
        }
    }
}

fn of_sequence(id: &BinId, s: &Sequence, answers: &Answers) -> Vec<MarkerInfo> {
    let mut marks: Vec<_> = s.markers.iter().collect();
    marks.sort_by(|a, b| a.t.total_cmp(&b.t));
    marks
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let mid = marker_id(id, m.t);
            let done = answers.get(&mid);
            let author = author(m);
            MarkerInfo {
                index: i + 1,
                sequence: id.clone(),
                t: m.t,
                label: m.label.clone(),
                color: m.color.clone(),
                origin: m.origin.clone(),
                author: author.to_string(),
                waiting: !m.label.trim().is_empty() && author != "agent" && done.is_none(),
                done: done.map(|d| d.revision),
                done_by: done.map(|d| d.by.clone()),
                id: mid,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn markers(p: &Project, seq: &str) -> Vec<Marker> {
        p.seq(seq).unwrap().markers.clone()
    }

    fn set_markers(p: &mut Project, seq: &str, m: serde_json::Value) {
        let BinSource::Sequence(s) = &mut p.bin.get_mut(seq).unwrap().source else { unreachable!() };
        s.markers = serde_json::from_value(m).unwrap();
    }

    #[test]
    fn markers_are_listed_by_where_they_are_with_who_left_them() {
        assert_eq!(marker_id("master", 8.6), "master@8.600");
        assert_eq!(marker_id("intro", 0.0), "intro@0.000");
        let p: Project = serde_json::from_value(json!({
            "master": "main",
            "bin": {
                "a": { "source": { "tracks": [], "markers": [{ "t": 1.0, "label": "nested" }] } },
                "main": { "source": { "tracks": [], "markers": [
                    { "t": 8.6, "label": "HUD fights the subtitles", "color": "#f00", "origin": "ui" },
                    { "t": 2.0, "label": "" },
                    { "t": 1.25, "label": "too fast" },
                    { "t": 4.0, "label": "TODO: tighten", "origin": "mcp" }
                ] } }
            }
        }))
        .unwrap();
        let answers: Answers = [("main@8.600".to_string(), Done { revision: 3, by: "agent".into() })].into();
        let all = list(&p, &answers);
        let got: Vec<(&str, usize, &str, bool, Option<u64>)> = all.iter().map(|m| (m.id.as_str(), m.index, m.author.as_str(), m.waiting, m.done)).collect();
        assert_eq!(
            got,
            [
                ("main@1.250", 1, "user", true, None),
                ("main@2.000", 2, "user", false, None),
                ("main@4.000", 3, "agent", false, None),
                ("main@8.600", 4, "user", false, Some(3)),
                ("a@1.000", 1, "user", true, None)
            ],
            "the master first; an empty label, the agent's own and an answered one wait for nothing"
        );
        assert_eq!(all[3].done_by.as_deref(), Some("agent"));
        assert_eq!(all[3].color.as_deref(), Some("#f00"));
        assert_eq!((all[0].origin.as_deref(), all[2].origin.as_deref()), (None, Some("mcp")));
        assert!(exists(&p, "main@8.600") && exists(&p, "a@1.000") && exists(&p, "main@2.000"));
        assert!(!exists(&p, "main@9.000") && !exists(&p, "nowhere"), "absent, malformed");

        let mut q = p.clone();
        assert!(!differ(&p, &q));
        q.name = "renamed".into();
        assert!(!differ(&p, &q), "only markers count");
        let BinSource::Sequence(s) = &mut q.bin.get_mut("main").unwrap().source else { unreachable!() };
        s.markers[1].label = "now it says something".into();
        assert!(differ(&p, &q), "an empty label counts too");
        let mut r = p.clone();
        let BinSource::Sequence(s) = &mut r.bin.get_mut("main").unwrap().source else { unreachable!() };
        s.markers[0].origin = Some("mcp".into());
        assert!(differ(&p, &r), "and so does who left it");
    }

    /// A new marker is the adding edit's; one the edit kept, even rewritten without its
    /// origin, stays whoever's it was — an older, unattributed one included.
    #[test]
    fn a_marker_is_stamped_with_the_edit_that_added_it() {
        let before: Project = serde_json::from_value(json!({
            "master": "main",
            "bin": { "main": { "source": { "tracks": [], "markers": [
                { "t": 1.0, "label": "too dark", "origin": "ui" },
                { "t": 2.0, "label": "an older note" }
            ] } } }
        }))
        .unwrap();
        let mut after = before.clone();
        set_markers(&mut after, "main", json!([
            { "t": 1.0, "label": "far too dark" },
            { "t": 2.0, "label": "an older note" },
            { "t": 3.0, "label": "TODO: the zone starts here" },
            { "t": 4.0, "label": "", "origin": "cli" }
        ]));
        stamp(&before, &mut after, "mcp: mark the zone");
        let origins: Vec<_> = markers(&after, "main").into_iter().map(|m| m.origin).collect();
        assert_eq!(origins, [Some("ui".into()), None, Some("mcp".into()), Some("cli".into())]);

        let mut fresh = before.clone();
        set_markers(&mut fresh, "main", json!([{ "t": 5.0, "label": "louder" }]));
        stamp(&before, &mut fresh, "");
        assert_eq!(markers(&fresh, "main")[0].origin.as_deref(), Some("ui"), "the editor's default");
        stamp(&before, &mut fresh, "mcp");
        assert_eq!(markers(&fresh, "main")[0].origin.as_deref(), Some("ui"), "stamped once");
    }

    /// What is saved is read back, less the answers whose marker is gone; no file, or one
    /// that does not parse, is no answers.
    #[test]
    fn answers_are_kept_for_the_markers_still_there() {
        let p: Project = serde_json::from_value(json!({
            "master": "main",
            "bin": { "main": { "source": { "tracks": [], "markers": [{ "t": 1.0, "label": "too dark" }] } } }
        }))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answers").join("t.kuma.json");
        assert!(load(&path, &p).is_empty(), "nothing kept yet");
        let answers: Answers = [
            ("main@1.000".to_string(), Done { revision: 4, by: "agent".into() }),
            ("main@2.000".to_string(), Done { revision: 5, by: "user".into() }),
        ]
        .into();
        save(&path, &answers).unwrap();
        let back = load(&path, &p);
        assert_eq!(back.len(), 1, "the marker at 2s is gone");
        assert_eq!(back["main@1.000"], Done { revision: 4, by: "agent".into() });
        std::fs::write(&path, "not json").unwrap();
        assert!(load(&path, &p).is_empty());
    }
}
