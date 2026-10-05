//! What the agent is doing, as it does it.
//!
//! The editor sees an agent's edits as patches land, but not the calls between them — the
//! frames it sampled, the overlay it checked, the call that is running now. The MCP server
//! reports each tool call here when it starts and when it ends, and the daemon keeps the
//! last few in memory and broadcasts them. Session state: never in the document, and a
//! report that never arrives costs only this list, never the tool.

use std::collections::VecDeque;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Records kept: a session's recent past, not a log.
pub const RECORDS: usize = 100;
/// Frame thumbnails listed per call.
const MAX_FRAMES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Done,
    Failed,
}

/// One tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: u64,
    pub tool: String,
    /// The MCP client's name (`claude-code`), or `agent`.
    pub client: String,
    pub state: State,
    /// As the MCP server trimmed them: a patch is its op count.
    pub args: Value,
    /// `apply_edits`' note.
    pub summary: Option<String>,
    pub revision_before: u64,
    pub revision_after: Option<u64>,
    /// Milliseconds since the epoch.
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub error: Option<String>,
    /// What a `sample_frames` call looked at.
    pub sample: Option<Sample>,
    /// A `check_overlay` call's verdict, `{ok, errors, warnings}`.
    pub lint: Option<Value>,
    pub pins: Vec<String>,
    /// The frames a finished `sample_frames` call produced, as paths under
    /// `.kuma/samples/` (served by `GET /api/samples/…`): its contact sheet alone, or up
    /// to eight frames.
    pub frames: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub sequence: Option<String>,
    pub overlay: Option<String>,
    pub times: Vec<f64>,
    pub rect: Option<Value>,
}

/// `POST /api/activity`.
#[derive(Debug, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Report {
    Start {
        tool: String,
        #[serde(default)]
        client: Option<String>,
        #[serde(default)]
        args: Value,
    },
    End {
        id: u64,
        ok: bool,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        result: Value,
    },
}

#[derive(Default)]
pub struct Activity {
    next: u64,
    records: VecDeque<Record>,
}

impl Activity {
    /// A call has started, at `revision`.
    pub fn start(&mut self, tool: String, client: Option<String>, args: Value, revision: u64, now: u64) -> Record {
        self.next += 1;
        let r = Record {
            id: self.next,
            summary: (tool == "apply_edits").then(|| args.get("note").and_then(Value::as_str).map(str::to_string)).flatten(),
            sample: (tool == "sample_frames").then(|| sample_of(&args)).flatten(),
            pins: args.get("pins").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
            client: client.filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "agent".into()),
            tool,
            state: State::Running,
            args,
            revision_before: revision,
            revision_after: None,
            started_at: now,
            ended_at: None,
            error: None,
            lint: None,
            frames: Vec::new(),
        };
        if self.records.len() == RECORDS {
            self.records.pop_front();
        }
        self.records.push_back(r.clone());
        r
    }

    /// Call `id` has ended, at `revision`: with its result summary, or failed with its error
    /// (a failed call has no verdict or frames to show). `None` when it is not (or no
    /// longer) kept.
    pub fn end(&mut self, id: u64, outcome: Result<&Value, Option<String>>, revision: u64, now: u64, samples_dir: &Path) -> Option<Record> {
        let r = self.records.iter_mut().find(|r| r.id == id)?;
        r.revision_after = Some(revision);
        r.ended_at = Some(now);
        match outcome {
            Ok(result) => {
                r.state = State::Done;
                if r.tool == "check_overlay" && result.is_object() {
                    r.lint = Some(result.clone());
                }
                if r.tool == "sample_frames" {
                    r.frames = frames_of(result, samples_dir);
                }
            }
            Err(error) => {
                r.state = State::Failed;
                r.error = Some(error.unwrap_or_else(|| "failed".into()));
            }
        }
        Some(r.clone())
    }

    /// The last `limit` calls, newest first.
    pub fn newest(&self, limit: usize) -> Vec<Record> {
        self.records.iter().rev().take(limit).cloned().collect()
    }
}

/// The moments a `sample_frames` call asked for: its `times`, else `t0` and `t1` if given.
/// A call resuming an earlier sample (`job`) looks at nothing new.
fn sample_of(args: &Value) -> Option<Sample> {
    if args.get("job").is_some_and(|j| !j.is_null()) {
        return None;
    }
    let str_of = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let listed: Vec<f64> = args.get("times").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_f64).collect();
    let times = if listed.is_empty() {
        let t0 = args.get("t0").and_then(Value::as_f64).unwrap_or(0.0);
        std::iter::once(t0).chain(args.get("t1").and_then(Value::as_f64)).collect()
    } else {
        listed
    };
    Some(Sample { sequence: str_of("sequence"), overlay: str_of("overlay"), times, rect: args.get("rect").filter(|r| r.is_object()).cloned() })
}

/// A finished sample's files, relative to `samples_dir`: the sheet alone when there is one,
/// else the frames. Anything outside the folder is not something the route would serve.
fn frames_of(result: &Value, samples_dir: &Path) -> Vec<String> {
    let rel = |p: &Value| {
        let p = Path::new(p.as_str()?);
        let rel = p.strip_prefix(samples_dir).ok()?;
        rel.components().all(|c| matches!(c, std::path::Component::Normal(_))).then(|| rel.to_string_lossy().replace('\\', "/"))
    };
    match result.get("sheet").filter(|s| s.is_string()) {
        Some(sheet) => rel(sheet).into_iter().collect(),
        None => result.get("paths").and_then(Value::as_array).into_iter().flatten().filter_map(rel).take(MAX_FRAMES).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_sample_says_which_moments_it_looked_at() {
        let s = |args: Value| sample_of(&args);
        assert_eq!(s(json!({ "t0": 46.0, "t1": 49.0 })).unwrap().times, [46.0, 49.0]);
        assert_eq!(s(json!({ "t0": 3.0, "times": [1.0, 2.0] })).unwrap().times, [1.0, 2.0]);
        let one = s(json!({ "overlay": "ov", "rect": { "x": 0, "y": 0, "w": 960, "h": 540 } })).unwrap();
        assert_eq!((one.times, one.overlay.as_deref(), one.sequence), (vec![0.0], Some("ov"), None));
        assert!(one.rect.is_some());
        assert!(s(json!({ "job": 4 })).is_none());
    }

    #[test]
    fn frames_are_named_inside_the_samples_folder() {
        let dir = Path::new("/p/.kuma/samples");
        let paths = json!({ "paths": ["/p/.kuma/samples/a/1.png", "/elsewhere/x.png", "/p/.kuma/samples/2.png"], "sheet": null });
        assert_eq!(frames_of(&paths, dir), ["a/1.png", "2.png"]);
        let sheet = json!({ "paths": ["/p/.kuma/samples/1.png"], "sheet": "/p/.kuma/samples/sheet.png" });
        assert_eq!(frames_of(&sheet, dir), ["sheet.png"]);
        let many: Vec<String> = (0..20).map(|i| format!("/p/.kuma/samples/{i}.png")).collect();
        assert_eq!(frames_of(&json!({ "paths": many }), dir).len(), MAX_FRAMES);
    }

    #[test]
    fn calls_are_kept_newest_first_and_bounded() {
        let mut a = Activity::default();
        let dir = Path::new("/s");
        let first = a.start("apply_edits".into(), Some("claude-code".into()), json!({ "note": "trim", "pins": ["main@1.000"] }), 4, 10);
        assert_eq!((first.summary.as_deref(), first.pins.as_slice(), first.state), (Some("trim"), &["main@1.000".to_string()][..], State::Running));
        let ended = a.end(first.id, Err(Some("409".into())), 4, 11, dir).unwrap();
        assert_eq!((ended.state, ended.error.as_deref(), ended.revision_after), (State::Failed, Some("409"), Some(4)));
        for _ in 0..RECORDS {
            a.start("health".into(), None, Value::Null, 4, 12);
        }
        let all = a.newest(usize::MAX);
        assert_eq!((all.len(), all[0].id, all[0].client.as_str()), (RECORDS, RECORDS as u64 + 1, "agent"));
        assert!(a.end(first.id, Ok(&Value::Null), 5, 13, dir).is_none(), "evicted");
    }
}
