//! What the agent is doing, as it does it.
//!
//! The editor sees an agent's edits as patches land, but not the calls between them — the
//! frames it sampled, the overlay it checked, the render that is running now. The daemon
//! keeps the last few calls in memory and broadcasts them, and owns every record's life:
//! a call that starts a job (`kuma mcp` stamps the request with `origin: "mcp: <tool>"`
//! and `call`, and only a job the agent asked for is followed here) is recorded when the
//! job is created and finished when the job is ([`Activity::from_job`],
//! [`Activity::job_ended`], both in `jobs::spawn_inner`), and any other call is reported
//! once, after it has ended ([`Report`]). So nothing waits on a report and no record waits
//! on one: a client that cancels, dies or loses a report cannot leave a call running.
//! Session state: never in the document, and a report that never arrives costs only this
//! list, never the tool.

use std::collections::VecDeque;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::jobs::{Job, JobState};

/// Records kept: a session's recent past, not a log.
pub const RECORDS: usize = 100;
/// Frame thumbnails listed per call.
const MAX_FRAMES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Only a call whose job has not ended: every other call is recorded finished.
    Running,
    Done,
    Failed,
    /// Its job was cancelled (`cancel_job`, or from the editor's queue).
    Cancelled,
}

/// The tool call that asked for a job, as `kuma mcp` stamps a job's request (`call`) and
/// the job keeps it (`asked_by`'s `tool` and `client`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub tool: String,
    /// The MCP client's name (`claude-code`); `agent` when it gave none.
    #[serde(default = "agent", deserialize_with = "client_name")]
    pub client: String,
}

fn agent() -> String {
    "agent".into()
}

/// A client's name, `agent` for a missing, null or blank one.
fn client_name<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.filter(|c| !c.trim().is_empty()).unwrap_or_else(agent))
}

/// One tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: u64,
    pub tool: String,
    /// The MCP client's name (`claude-code`), or `agent`.
    pub client: String,
    pub state: State,
    /// As the MCP server trimmed them (a patch is its op count); for a call that started a
    /// job, the job's `params`.
    pub args: Value,
    /// `apply_edits`' note.
    pub summary: Option<String>,
    pub revision_before: u64,
    /// Set when it ends: the revision it made, or `revision_before` when it made none, so
    /// the two differ exactly when the call edited — whatever else landed meanwhile.
    pub revision_after: Option<u64>,
    /// Milliseconds since the epoch.
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub error: Option<String>,
    /// What a `sample_frames` call looked at.
    pub sample: Option<Sample>,
    /// A `check_overlay` call's verdict, `{ok, errors, warnings}`.
    pub lint: Option<Value>,
    /// The frames a finished `sample_frames` call produced, as paths under
    /// `.kuma/samples/` (served by `GET /api/samples/…`): its contact sheet alone, or up
    /// to eight frames.
    pub frames: Vec<String>,
    /// The job the call started, which it runs and ends with.
    pub job: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub sequence: Option<String>,
    pub overlay: Option<String>,
    pub times: Vec<f64>,
    pub rect: Option<Value>,
}

/// `POST /api/activity`: a call that started no job, once it has ended. Anything else (the
/// `start` and `end` an older `kuma mcp` sends) is refused, and costs that server's feed.
#[derive(Debug, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Report {
    Call {
        tool: String,
        #[serde(default = "agent", deserialize_with = "client_name")]
        client: String,
        #[serde(default)]
        args: Value,
        ok: bool,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        result: Value,
        /// The revision the call made, read off its own answer; absent for a call that
        /// edited nothing.
        #[serde(default)]
        revision: Option<u64>,
        /// When the call started, in milliseconds since the epoch: the report is sent
        /// when it has ended. Absent reads as now.
        #[serde(default)]
        started_at: Option<u64>,
    },
}

/// How a call ended.
enum Outcome<'a> {
    Done(&'a Value),
    Failed(Option<String>),
    Cancelled,
}

#[derive(Default)]
pub struct Activity {
    next: u64,
    records: VecDeque<Record>,
}

impl Activity {
    fn record(&mut self, tool: String, client: String, args: Value, revision_before: u64, started_at: u64) -> Record {
        self.next += 1;
        Record {
            id: self.next,
            summary: (tool == "apply_edits").then(|| args.get("note").and_then(Value::as_str).map(str::to_string)).flatten(),
            sample: (tool == "sample_frames").then(|| sample_of(&args)).flatten(),
            client,
            tool,
            state: State::Running,
            args,
            revision_before,
            revision_after: None,
            started_at,
            ended_at: None,
            error: None,
            lint: None,
            frames: Vec::new(),
            job: None,
        }
    }

    /// Keep `r`, forgetting the oldest finished record when full: a call still running is
    /// kept until it ends (unless every record kept is one).
    fn keep(&mut self, r: Record) -> Record {
        if self.records.len() >= RECORDS {
            let oldest = self.records.iter().position(|r| r.state != State::Running).unwrap_or(0);
            self.records.remove(oldest);
        }
        self.records.push_back(r.clone());
        r
    }

    /// A call that started no job has ended, as `report` says; the project is at
    /// `current`. It is kept finished: its `revision_before` is the one before the revision
    /// it made, or `current` when it made none.
    pub fn call(&mut self, report: Report, current: u64, now: u64, samples_dir: &Path) -> Record {
        let Report::Call { tool, client, args, ok, error, result, revision, started_at } = report;
        let before = revision.map_or(current, |r| r.saturating_sub(1));
        let mut r = self.record(tool, client, args, before, started_at.unwrap_or(now).min(now));
        finish(&mut r, if ok { Outcome::Done(&result) } else { Outcome::Failed(error) }, revision, now, samples_dir);
        self.keep(r)
    }

    /// `job` has just been created, asked for by `call` at `revision`: its record runs
    /// until the job ends ([`Activity::job_ended`]). What it was asked is the job's params.
    pub fn from_job(&mut self, job: &Job, call: &Call, revision: u64, now: u64) -> Record {
        let mut r = self.record(call.tool.clone(), call.client.clone(), job.params.clone(), revision, now);
        r.job = Some(job.id);
        self.keep(r)
    }

    /// `job` is final: finish the record that follows it, with the revision its result
    /// names (a transcription's). `None` when no call asked for it, or its record is gone.
    pub fn job_ended(&mut self, job: &Job, now: u64, samples_dir: &Path) -> Option<Record> {
        let r = self.records.iter_mut().find(|r| r.job == Some(job.id) && r.state == State::Running)?;
        let outcome = match job.state {
            JobState::Done => Outcome::Done(&job.result),
            JobState::Cancelled => Outcome::Cancelled,
            _ => Outcome::Failed(Some(job.message.clone())),
        };
        let revision = (job.state == JobState::Done).then(|| job.result.get("revision").and_then(Value::as_u64)).flatten();
        finish(r, outcome, revision, now, samples_dir);
        Some(r.clone())
    }

    /// The last `limit` calls, newest first. Borrowed: a list is serialised as it is read.
    pub fn newest(&self, limit: usize) -> Vec<&Record> {
        self.records.iter().rev().take(limit).collect()
    }
}

/// `r` has ended, having made `revision` (`None`: none): with what its result shows, or
/// failed with its error (a failed or cancelled call has no verdict or frames to show).
fn finish(r: &mut Record, outcome: Outcome, revision: Option<u64>, now: u64, samples_dir: &Path) {
    r.revision_after = Some(revision.unwrap_or(r.revision_before));
    r.ended_at = Some(now);
    match outcome {
        Outcome::Done(result) => {
            r.state = State::Done;
            if r.tool == "check_overlay" && result.is_object() {
                r.lint = Some(result.clone());
            }
            if r.tool == "sample_frames" {
                r.frames = frames_of(result, samples_dir);
            }
        }
        Outcome::Failed(error) => {
            r.state = State::Failed;
            r.error = Some(error.unwrap_or_else(|| "failed".into()));
        }
        Outcome::Cancelled => r.state = State::Cancelled,
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
/// else the frames — `paths` as `kuma mcp` reports a call, or each of `samples` as a sample
/// job's result lists them. Anything outside the folder is not something the route would
/// serve.
fn frames_of(result: &Value, samples_dir: &Path) -> Vec<String> {
    let rel = |p: &Value| {
        let p = Path::new(p.as_str()?);
        let rel = p.strip_prefix(samples_dir).ok()?;
        rel.components().all(|c| matches!(c, std::path::Component::Normal(_))).then(|| rel.to_string_lossy().replace('\\', "/"))
    };
    match result.get("sheet").filter(|s| s.is_string()) {
        Some(sheet) => rel(sheet).into_iter().collect(),
        None => match result.get("paths").and_then(Value::as_array) {
            Some(paths) => paths.iter().filter_map(rel).take(MAX_FRAMES).collect(),
            None => result.get("samples").and_then(Value::as_array).into_iter().flatten().filter_map(|s| rel(&s["path"])).take(MAX_FRAMES).collect(),
        },
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

    fn reported(tool: &str, ok: bool, result: Value, revision: Option<u64>) -> Report {
        let error = (!ok).then(|| "409: stale".to_string());
        Report::Call { tool: tool.into(), client: agent(), args: json!({ "note": "trim" }), ok, error, result, revision, started_at: Some(10) }
    }

    fn job(id: u64, state: JobState, result: Value) -> Job {
        let message = if state == JobState::Failed { "no melt" } else { "" };
        Job { id, kind: "sample".into(), state, percent: 0, message: message.into(), result, cancellable: true, params: json!({ "t0": 1.0 }), asked_by: None }
    }

    /// A call that started no job is kept finished, credited with the revision it made
    /// (the one before it is its `revision_before`) or none.
    #[test]
    fn a_reported_call_is_kept_finished() {
        let mut a = Activity::default();
        let dir = Path::new("/s");
        let edit = a.call(reported("apply_edits", true, Value::Null, Some(8)), 9, 11, dir);
        assert_eq!((edit.state, edit.summary.as_deref(), edit.revision_before, edit.revision_after), (State::Done, Some("trim"), 7, Some(8)));
        assert_eq!((edit.started_at, edit.ended_at, edit.job), (10, Some(11), None));
        let refused = a.call(reported("apply_edits", false, Value::Null, None), 9, 12, dir);
        assert_eq!((refused.state, refused.error.as_deref(), refused.revision_before, refused.revision_after), (State::Failed, Some("409: stale"), 9, Some(9)));
        let parsed: Report = serde_json::from_value(json!({ "phase": "call", "tool": "health", "client": null, "ok": true })).unwrap();
        assert_eq!(a.call(parsed, 9, 13, dir).client, "agent");
        assert!(serde_json::from_value::<Report>(json!({ "phase": "start", "tool": "health" })).is_err(), "the old protocol");
    }

    /// A job a call asked for runs its record from creation to its end, whichever end.
    #[test]
    fn a_job_record_follows_its_job() {
        let mut a = Activity::default();
        let dir = Path::new("/p/.kuma/samples");
        let call = Call { tool: "sample_frames".into(), client: "claude-code".into() };
        let started = a.from_job(&job(4, JobState::Running, Value::Null), &call, 3, 20);
        assert_eq!((started.state, started.job, started.revision_before, &started.args), (State::Running, Some(4), 3, &json!({ "t0": 1.0 })));
        assert_eq!(started.sample.unwrap().times, [1.0]);
        let result = json!({ "samples": [{ "t": 1.0, "path": "/p/.kuma/samples/a.png" }], "sheet": null });
        let done = a.job_ended(&job(4, JobState::Done, result), 21, dir).unwrap();
        assert_eq!((done.state, done.revision_after, done.ended_at, done.frames), (State::Done, Some(3), Some(21), vec!["a.png".to_string()]));
        assert!(a.job_ended(&job(4, JobState::Done, Value::Null), 22, dir).is_none(), "ends once");

        let transcribe = Call { tool: "transcribe".into(), client: "agent".into() };
        a.from_job(&job(5, JobState::Running, Value::Null), &transcribe, 3, 20);
        assert_eq!(a.job_ended(&job(5, JobState::Done, json!({ "revision": 9 })), 23, dir).unwrap().revision_after, Some(9));
        a.from_job(&job(6, JobState::Queued, Value::Null), &call, 3, 20);
        assert_eq!(a.job_ended(&job(6, JobState::Cancelled, Value::Null), 24, dir).unwrap().state, State::Cancelled);
        a.from_job(&job(7, JobState::Running, Value::Null), &call, 3, 20);
        let failed = a.job_ended(&job(7, JobState::Failed, Value::Null), 25, dir).unwrap();
        assert_eq!((failed.state, failed.error.as_deref(), failed.frames.len()), (State::Failed, Some("no melt"), 0));
        assert!(a.job_ended(&job(8, JobState::Done, Value::Null), 26, dir).is_none(), "no call asked for it");
    }

    #[test]
    fn calls_are_kept_newest_first_and_bounded() {
        let mut a = Activity::default();
        let dir = Path::new("/s");
        let call = Call { tool: "render".into(), client: "claude-code".into() };
        let long = a.from_job(&job(1, JobState::Running, Value::Null), &call, 4, 10);
        let first = a.call(reported("apply_edits", true, Value::Null, None), 4, 11, dir);
        for _ in 0..RECORDS {
            a.call(reported("health", true, Value::Null, None), 4, 12, dir);
        }
        let all = a.newest(usize::MAX);
        assert_eq!((all.len(), all[0].id, all[0].client.as_str()), (RECORDS, RECORDS as u64 + 2, "agent"));
        assert!(all.iter().all(|r| r.id != first.id), "the oldest finished call is forgotten");
        assert!(all.iter().any(|r| r.id == long.id), "a running one is not");
        assert_eq!(a.job_ended(&job(1, JobState::Done, Value::Null), 13, dir).unwrap().state, State::Done);
    }
}
