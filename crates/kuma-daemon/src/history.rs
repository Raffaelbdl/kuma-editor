//! The revision history: who changed what, in one list for every client.
//!
//! The undo stack holds whole documents and says nothing about them; this is the readable
//! side of the same story — one entry per revision the daemon made, plus the patches it
//! refused, each with its author and the clips and entries it touched. It is session state:
//! never written into the document and gone with the daemon, like the undo stack itself.
//!
//! Snapshots of recent revisions live here too, so any of them can be restored as one new,
//! undoable edit rather than by undoing everything since.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use kuma_core::edit::EditError;
use json_patch::jsonptr::Pointer;
use kuma_core::{BinItem, BinSource, Clip, Project, Sequence};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Entries kept: enough for a long session's list, small enough to send whole.
pub const ENTRIES: usize = 500;
/// Revisions that can be restored. Whole documents, so fewer than the entries.
pub const SNAPSHOTS: usize = 200;

/// A whole document as compact JSON: how the history and the undo stack keep past revisions.
/// A typed [`Project`] takes several times its JSON in memory — a few hundred clips and a
/// long transcript are over a megabyte each, kept hundreds of times — and these are read
/// only when someone goes back. Shared, so a revision both of them keep is held once.
pub type Snapshot = Arc<str>;

/// `p` as a [`Snapshot`].
pub fn snapshot(p: &Project) -> serde_json::Result<Snapshot> {
    serde_json::to_string(p).map(Snapshot::from)
}

/// The document a [`Snapshot`] holds. One this daemon made, so it is current: no upgrade.
pub fn parse(s: &str) -> serde_json::Result<Project> {
    serde_json::from_str(s)
}
/// Field changes listed per entry: what a row in a list can show, not a diff.
const MAX_CHANGES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Edit,
    Undo,
    Redo,
    Restore,
    /// A patch the daemon would not apply: listed so the person sees what the agent tried.
    Refused,
}

/// One revision, or one refusal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// The revision it made; `None` for a refusal, which made none.
    pub revision: Option<u64>,
    pub kind: Kind,
    /// As the edit was labelled (`mcp: …`, `ui`), or `undo:<label>` for a step.
    pub origin: String,
    /// `agent`, `user` or `other` ([`author`]).
    pub author: String,
    pub summary: String,
    pub ops: usize,
    /// Milliseconds since the epoch.
    pub at: u64,
    pub base_revision: Option<u64>,
    pub error: Option<String>,
    /// Clip ids and bin ids it touched.
    pub targets: Vec<String>,
    pub changes: Vec<Change>,
    /// The pins this edit answers (`crate::pins`).
    pub pins: Vec<String>,
}

impl Entry {
    /// An entry with what follows from its origin and targets filled in, made now; the
    /// rest (ops, base, error, changes, pins) is the caller's to set.
    pub fn new(kind: Kind, origin: &str, revision: Option<u64>, targets: &[Target]) -> Entry {
        Entry {
            revision,
            kind,
            origin: origin.to_string(),
            author: author(origin).to_string(),
            summary: summary(origin),
            ops: 0,
            at: now_ms(),
            base_revision: None,
            error: None,
            targets: targets.iter().map(|t| t.id().to_string()).collect(),
            changes: Vec::new(),
            pins: Vec::new(),
        }
    }
}

/// A scalar field of a clip or bin entry that an edit changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub target: String,
    pub field: String,
    pub from: Value,
    pub to: Value,
}

#[derive(Default)]
pub struct History {
    entries: VecDeque<Entry>,
    snapshots: BTreeMap<u64, Snapshot>,
}

impl History {
    pub fn push(&mut self, e: Entry) {
        if self.entries.len() == ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }

    /// Keep `doc` as the document at `revision`, dropping the oldest beyond [`SNAPSHOTS`].
    pub fn keep(&mut self, revision: u64, doc: Snapshot) {
        self.snapshots.insert(revision, doc);
        while self.snapshots.len() > SNAPSHOTS {
            self.snapshots.pop_first();
        }
    }

    /// The document as it was at `revision`, if it is still kept. A shared handle, so it is
    /// parsed after the history is let go of.
    pub fn snapshot(&self, revision: u64) -> Option<Snapshot> {
        self.snapshots.get(&revision).cloned()
    }

    /// The last `limit` entries, newest first. Borrowed: a list is serialised as it is read.
    pub fn newest(&self, limit: usize) -> Vec<&Entry> {
        self.entries.iter().rev().take(limit).collect()
    }
}

/// Who an origin speaks for — an edit's, or a job's. The editor labels its own `ui…`,
/// `kuma mcp` its `mcp: …`; anything else, the command line's `cli: …` among them, is
/// `other`.
pub fn author(origin: &str) -> &'static str {
    let head = origin.split([':', ' ']).next().unwrap_or_default();
    match head {
        "ui" => "user",
        "mcp" | "agent" => "agent",
        _ => "other",
    }
}

/// What an edit was, for a person: its origin without the `who: ` in front. A bare `ui`
/// (the editor's default) says nothing more than that it was an edit.
pub fn summary(origin: &str) -> String {
    let rest = match origin.split_once(": ") {
        Some((who, rest)) if !who.contains(char::is_whitespace) => rest,
        _ => origin,
    };
    match rest.trim() {
        "" | "ui" | "mcp" | "agent" => "edit".into(),
        s => s.to_string(),
    }
}

/// What a refusal says in the list, shorter than the 409/422 body.
pub fn refusal(e: &EditError) -> String {
    match e {
        EditError::RevisionMismatch { current, base } => format!("stale base r{base} (current r{current})"),
        EditError::Invalid(issues) => {
            let errors: Vec<&str> = issues.iter().filter(|i| i.severity == kuma_core::Severity::Error).map(|i| i.message.as_str()).take(3).collect();
            format!("validation failed: {}", errors.join("; "))
        }
        other => other.to_string(),
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// What an edit touched. Clips and bin entries share no namespace guarantee, so which one a
/// target is travels with it until it is printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Clip(String),
    Bin(String),
}

impl Target {
    pub fn id(&self) -> &str {
        match self {
            Target::Clip(id) | Target::Bin(id) => id,
        }
    }
}

/// The JSON pointer's tokens, unescaped.
fn tokens(pointer: &Pointer) -> Vec<String> {
    pointer.tokens().map(|t| t.decoded().into_owned()).collect()
}

/// The clip or entry a pointer lands in, in `doc`: `/bin/<seq>/source/tracks/<i>/clips/<j>…`
/// is clip `j`'s id (`-` is the last), any other `/bin/<id>…` is entry `<id>`, and the rest
/// of the document (`/width`, `/master`) is nobody's.
fn resolve(doc: &Project, pointer: &Pointer) -> Option<Target> {
    let t = tokens(pointer);
    let (first, id) = (t.first()?, t.get(1)?);
    if first != "bin" {
        return None;
    }
    if let [_, _, source, tracks, i, clips, j, ..] = t.as_slice() {
        if source == "source" && tracks == "tracks" && clips == "clips" {
            let track = i.parse::<usize>().ok().and_then(|i| doc.seq(id)?.tracks.get(i));
            let clip = track.and_then(|tr| if j == "-" { tr.clips.last() } else { tr.clips.get(j.parse::<usize>().ok()?) });
            if let Some(c) = clip {
                return Some(Target::Clip(c.id.clone()));
            }
        }
    }
    Some(Target::Bin(id.clone()))
}

/// The targets of a patch: what it read from in the document before (a replace, a remove,
/// a move's source) and what it wrote into the document after (an add, a move's
/// destination). A clip added whole is named by its own `id`, which survives later ops in
/// the same patch shifting indexes. In order, each once.
pub fn patch_targets(before: &Project, after: Option<&Project>, ops: &[json_patch::PatchOperation]) -> Vec<Target> {
    use json_patch::{CopyOperation, MoveOperation, PatchOperation as Op, RemoveOperation, ReplaceOperation};
    let mut out = Vec::new();
    for op in ops {
        let found = match op {
            Op::Add(a) => {
                let tk = tokens(&a.path);
                let whole_clip = tk.len() == 7 && tk[0] == "bin" && tk[2] == "source" && tk[3] == "tracks" && tk[5] == "clips";
                match a.value.get("id").and_then(Value::as_str) {
                    Some(id) if whole_clip => vec![Some(Target::Clip(id.to_string()))],
                    _ => vec![resolve(after.unwrap_or(before), &a.path)],
                }
            }
            Op::Remove(RemoveOperation { path }) | Op::Replace(ReplaceOperation { path, .. }) => vec![resolve(before, path)],
            Op::Move(MoveOperation { from, path }) | Op::Copy(CopyOperation { from, path }) => {
                vec![resolve(before, from), resolve(after.unwrap_or(before), path)]
            }
            Op::Test(_) => vec![],
        };
        for t in found.into_iter().flatten() {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// A document's clips by id.
type Clips<'a> = BTreeMap<&'a str, &'a Clip>;

/// Every clip of `p` by id, or only those `only` names, found in one pass.
fn clips_by_id<'a>(p: &'a Project, only: Option<&BTreeSet<&str>>) -> Clips<'a> {
    p.all_clips().filter(|(_, _, c)| only.is_none_or(|ids| ids.contains(c.id.as_str()))).map(|(_, _, c)| (c.id.as_str(), c)).collect()
}

/// A bin entry as what it is beside its clips: a sequence's tracks are those, which are
/// diffed one by one, and would otherwise make every sequence "changed" by any clip edit.
/// Borrowed, so comparing two costs no copy and no JSON.
fn entry_parts(e: &BinItem) -> impl PartialEq + '_ {
    let BinItem { name, source, meta, parent, order, volume } = e;
    let source = match source {
        BinSource::Sequence(Sequence { tracks: _, markers, effects }) => Err((markers, effects)),
        other => Ok(other),
    };
    (name, source, meta, parent, order, volume)
}

/// What a bin entry's fields are as JSON for [`changes`], its source left out: that is
/// never a scalar field, and a sequence's is all its tracks. Serialised as the entry is,
/// so a field written only when it is not its default is absent here too.
fn entry_value(e: &BinItem) -> Option<Value> {
    let BinItem { name, source: _, meta, parent, order, volume } = e;
    let stand_in = BinSource::Folder { folder: true };
    let mut v = serde_json::to_value(BinItem { name: name.clone(), source: stand_in, meta: meta.clone(), parent: parent.clone(), order: *order, volume: *volume }).ok()?;
    v.as_object_mut()?.remove("source");
    Some(v)
}

/// What changed between two whole documents, for a step that swaps one for the other
/// (undo, redo, restore): clips added, removed or changed, by id, then bin entries likewise.
pub fn diff_targets(before: &Project, after: &Project) -> Vec<Target> {
    targets_between(before, after, &clips_by_id(before, None), &clips_by_id(after, None))
}

/// [`diff_targets`] and the [`changes`] on them, for a step that swaps documents: every clip
/// is indexed once for both.
pub fn diff(before: &Project, after: &Project) -> (Vec<Target>, Vec<Change>) {
    let (a, b) = (clips_by_id(before, None), clips_by_id(after, None));
    let targets = targets_between(before, after, &a, &b);
    let changes = changes_between(before, after, &a, &b, &targets);
    (targets, changes)
}

fn targets_between(before: &Project, after: &Project, a: &Clips, b: &Clips) -> Vec<Target> {
    let mut ids: Vec<&str> = a.keys().chain(b.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out: Vec<Target> = ids.into_iter().filter(|id| a.get(id) != b.get(id)).map(|id| Target::Clip(id.to_string())).collect();
    let mut entries: Vec<&String> = before.bin.keys().chain(after.bin.keys()).collect();
    entries.sort_unstable();
    entries.dedup();
    out.extend(entries.into_iter().filter(|id| before.bin.get(*id).map(entry_parts) != after.bin.get(*id).map(entry_parts)).map(|id| Target::Bin(id.clone())));
    out
}

/// The top-level scalar fields that differ on each target present on both sides, at most
/// [`MAX_CHANGES`]. A field written only when it is not its default (`parent`, a clip's
/// `group`) reads as null where it is absent.
pub fn changes(before: &Project, after: &Project, targets: &[Target]) -> Vec<Change> {
    // Only the clips named: an edit touches a few of a timeline's hundreds.
    let ids: BTreeSet<&str> = targets.iter().filter_map(|t| if let Target::Clip(id) = t { Some(id.as_str()) } else { None }).collect();
    changes_between(before, after, &clips_by_id(before, Some(&ids)), &clips_by_id(after, Some(&ids)), targets)
}

/// [`changes`], with each side's clips already indexed (at least those `targets` names).
fn changes_between(before: &Project, after: &Project, a: &Clips, b: &Clips, targets: &[Target]) -> Vec<Change> {
    let scalar = |v: &Value| !v.is_object() && !v.is_array();
    // A target's object on one side, as JSON.
    let value_of = |p: &Project, clips: &Clips, t: &Target| match t {
        Target::Clip(id) => clips.get(id.as_str()).and_then(|c| serde_json::to_value(c).ok()),
        Target::Bin(id) => p.bin.get(id).and_then(entry_value),
    };
    let mut out = Vec::new();
    for t in targets {
        let (Some(Value::Object(x)), Some(Value::Object(y))) = (value_of(before, a, t), value_of(after, b, t)) else { continue };
        let mut fields: Vec<&String> = x.keys().chain(y.keys()).collect();
        fields.sort_unstable();
        fields.dedup();
        for f in fields {
            let (from, to) = (x.get(f).cloned().unwrap_or(Value::Null), y.get(f).cloned().unwrap_or(Value::Null));
            if from != to && scalar(&from) && scalar(&to) {
                out.push(Change { target: t.id().to_string(), field: f.clone(), from, to });
                if out.len() == MAX_CHANGES {
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project(clips: Value) -> Project {
        serde_json::from_value(json!({
            "bin": {
                "a": { "name": "A", "source": { "color": "#000000" } },
                "main": { "source": { "tracks": [{ "id": "v1", "clips": clips }] } }
            }
        }))
        .unwrap()
    }

    fn clip(id: &str, start: f64, end: f64) -> Value {
        json!({ "id": id, "start": start, "end": end, "bin": "a" })
    }

    fn ops(v: Value) -> Vec<json_patch::PatchOperation> {
        serde_json::from_value(v).unwrap()
    }

    fn ids(t: &[Target]) -> Vec<&str> {
        t.iter().map(Target::id).collect()
    }

    /// A clip is named by its id wherever the path lands in it; an entry by its bin id; the
    /// rest of the document by nobody. A clip added whole is named by the id it carries.
    #[test]
    fn a_patch_names_the_clips_and_entries_it_touches() {
        let before = project(json!([clip("c1", 0.0, 2.0), clip("c2", 2.0, 4.0)]));
        let patch = ops(json!([
            { "op": "replace", "path": "/bin/main/source/tracks/0/clips/1/start", "value": 2.5 },
            { "op": "replace", "path": "/bin/a/name", "value": "Black" },
            { "op": "add", "path": "/bin/main/source/tracks/0/clips/-", "value": clip("c3", 5.0, 6.0) },
            { "op": "remove", "path": "/bin/main/source/tracks/0/clips/0/effects" },
            { "op": "replace", "path": "/width", "value": 1280 },
            { "op": "replace", "path": "/bin/main/source/tracks/0/clips/1/end", "value": 4.5 }
        ]));
        let after = kuma_core::edit::apply_patch(&before, None, &patch, &Default::default()).unwrap().project;
        let t = patch_targets(&before, Some(&after), &patch);
        assert_eq!(ids(&t), ["c2", "a", "c3", "c1"]);
        assert_eq!(t[1], Target::Bin("a".into()));

        let ch = changes(&before, &after, &t);
        let got: Vec<(&str, &str, &Value, &Value)> = ch.iter().map(|c| (c.target.as_str(), c.field.as_str(), &c.from, &c.to)).collect();
        assert_eq!(got, [("c2", "end", &json!(4.0), &json!(4.5)), ("c2", "start", &json!(2.0), &json!(2.5)), ("a", "name", &json!("A"), &json!("Black"))]);
    }

    /// A step that swaps documents is diffed by id, and a sequence is not "changed" just
    /// because one of its clips was.
    #[test]
    fn two_documents_are_diffed_by_clip_id() {
        let before = project(json!([clip("c1", 0.0, 2.0), clip("c2", 2.0, 4.0)]));
        let after = project(json!([clip("c2", 2.0, 3.0), clip("c3", 3.0, 4.0)]));
        let t = diff_targets(&before, &after);
        assert_eq!(ids(&t), ["c1", "c2", "c3"]);
        let ch = [Change { target: "c2".into(), field: "end".into(), from: json!(4.0), to: json!(3.0) }];
        assert_eq!(changes(&before, &after, &t), ch);
        assert_eq!(diff(&before, &after), (t, ch.to_vec()), "one index for both says the same");
        assert!(diff_targets(&before, &before).is_empty());
    }

    #[test]
    fn origins_say_who_and_what() {
        assert_eq!((author("ui"), summary("ui")), ("user", "edit".to_string()));
        assert_eq!((author("ui: trim"), summary("ui: trim")), ("user", "trim".to_string()));
        assert_eq!((author("mcp: Moved + trimmed"), summary("mcp: Moved + trimmed")), ("agent", "Moved + trimmed".to_string()));
        assert_eq!((author("mcp"), summary("mcp")), ("agent", "edit".to_string()));
        assert_eq!((author("cli: render"), summary("cli: render")), ("other", "render".to_string()));
        assert_eq!(author("claude: an older kuma mcp"), "other");
        assert_eq!((author("test"), summary("test")), ("other", "test".to_string()));
        assert_eq!(refusal(&EditError::RevisionMismatch { current: 55, base: 53 }), "stale base r53 (current r55)");
    }

    #[test]
    fn snapshots_and_entries_are_bounded() {
        let mut h = History::default();
        for r in 0..(SNAPSHOTS as u64 + 5) {
            h.keep(r, snapshot(&Project { revision: r, ..Project::default() }).unwrap());
        }
        assert!(h.snapshot(4).is_none());
        assert_eq!(parse(&h.snapshot(5).unwrap()).unwrap().revision, 5, "a snapshot parses back into its document");
        for r in 0..(ENTRIES as u64 + 1) {
            h.push(Entry::new(Kind::Edit, "ui", Some(r), &[]));
        }
        let all = h.newest(usize::MAX);
        assert_eq!((all.len(), all[0].revision, all.last().unwrap().revision), (ENTRIES, Some(ENTRIES as u64), Some(1)));
    }
}
