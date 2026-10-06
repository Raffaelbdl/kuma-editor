//! What this server tells the daemon about each tool call, so the editor can show what the
//! agent is doing and not only the patches it lands (`POST /api/activity`).
//!
//! A report is small by construction: the arguments trimmed (a patch is its op count, long
//! strings and lists are cut), and the result reduced to the one thing the editor shows
//! for that tool, plus the revision a call that edits made. Everything here is pure;
//! sending is `Client::report`, best effort.

use rmcp::model::{CallToolResponse, CallToolResult};
use rmcp::ErrorData as McpError;
use serde_json::{json, Map, Value};

/// Longest string kept in reported arguments, in characters.
const MAX_STRING: usize = 200;
/// Most items kept of a reported list or object.
const MAX_ITEMS: usize = 32;
/// Deeper than this, a value is elided: reports name what was asked, not every var's cues.
const MAX_DEPTH: usize = 4;
/// Frame paths reported for a sample; the daemon keeps fewer.
const MAX_PATHS: usize = 32;

/// Tools that edit, and answer the revision they made ([`revision_made`]).
const EDITS: &[&str] = &["apply_edits", "undo", "redo", "add_blueprint", "add_media", "reprobe_media", "import_overlay", "make_overlay_unique", "transcribe"];
/// Tools [`summary`] reduces to more than whether they finished.
const SUMMARISED: &[&str] = &["check_overlay", "apply_edits", "sample_frames"];

/// The start of a call. A client that gave no name is the daemon's to name.
pub fn start(tool: &str, client: Option<&str>, args: Option<&Map<String, Value>>) -> Value {
    json!({ "phase": "start", "tool": tool, "client": client, "args": trim_args(args) })
}

/// The end of call `id`, from its response. Its answer is parsed only for a tool the
/// report reads something of.
pub fn end(id: u64, tool: &str, response: &Result<CallToolResponse, McpError>) -> Value {
    let (ok, error, answer) = match response {
        Ok(CallToolResponse::Complete(r)) if r.is_error == Some(true) => (false, first_text(r).map(str::to_string), None),
        Ok(CallToolResponse::Complete(r)) if SUMMARISED.contains(&tool) || EDITS.contains(&tool) => (true, None, first_text(r).and_then(|t| serde_json::from_str::<Value>(t).ok())),
        Ok(_) => (true, None, None),
        Err(e) => (false, Some(e.message.to_string()), None),
    };
    let (result, revision) = answer.map_or((Value::Null, None), |v| (summary(tool, &v), revision_made(tool, &v)));
    json!({ "phase": "end", "id": id, "ok": ok, "error": error, "result": result, "revision": revision })
}

/// A call's arguments, small: `patch` as `{"ops": n}` (its `note`, `base_revision` and
/// `pins` are kept beside it), and every string, list and object capped. Built from the
/// arguments as they are, never a copy of them whole.
pub fn trim_args(args: Option<&Map<String, Value>>) -> Value {
    let Some(args) = args else { return Value::Null };
    let trim = |(k, v): (&String, &Value)| {
        let v = match v.as_array() {
            Some(ops) if k == "patch" => json!({ "ops": ops.len() }),
            _ => cap(v, 1),
        };
        (k.clone(), v)
    };
    Value::Object(args.iter().take(MAX_ITEMS).map(trim).collect())
}

fn cap(v: &Value, depth: usize) -> Value {
    match v {
        Value::String(s) if s.chars().count() > MAX_STRING => Value::String(s.chars().take(MAX_STRING).chain(['…']).collect()),
        Value::Array(_) | Value::Object(_) if depth >= MAX_DEPTH => Value::String("…".into()),
        Value::Array(a) => Value::Array(a.iter().take(MAX_ITEMS).map(|x| cap(x, depth + 1)).collect()),
        Value::Object(o) => Value::Object(o.iter().take(MAX_ITEMS).map(|(k, x)| (k.clone(), cap(x, depth + 1))).collect::<Map<_, _>>()),
        other => other.clone(),
    }
}

fn first_text(r: &CallToolResult) -> Option<&str> {
    r.content.iter().find_map(|c| c.as_text().map(|t| t.text.as_str()))
}

/// The revision a call made, off the JSON it answered: `None` for a tool that does not
/// edit, a re-probe that found nothing to change, and a transcription still running. The
/// daemon reads `None` as "made none", so a call is never credited with an edit that
/// landed while it ran.
fn revision_made(tool: &str, v: &Value) -> Option<u64> {
    match tool {
        "reprobe_media" if !v["changed"].as_array().is_some_and(|c| !c.is_empty()) => None,
        "transcribe" => v["result"]["revision"].as_u64(),
        t if EDITS.contains(&t) => v["revision"].as_u64(),
        _ => None,
    }
}

/// What the editor shows of a finished call: a lint's verdict, an edit's revision, the
/// frames a sample made. Read off the JSON the tool answered; `null` for every other tool,
/// and for a sample still baking.
pub fn summary(tool: &str, v: &Value) -> Value {
    match tool {
        "check_overlay" => {
            let findings = v["findings"].as_array().map(Vec::as_slice).unwrap_or_default();
            let count = |level: &str| findings.iter().filter(|f| f["level"] == level).count();
            let errors = count("error");
            json!({ "ok": errors == 0, "errors": errors, "warnings": count("warning") })
        }
        "apply_edits" => json!({ "revision": v["revision"], "ops": v["ops"] }),
        "sample_frames" => match v["samples"].as_array() {
            Some(samples) => {
                let paths: Vec<&Value> = samples.iter().map(|s| &s["path"]).filter(|p| p.is_string()).take(MAX_PATHS).collect();
                json!({ "count": samples.len(), "paths": paths, "sheet": v["sheet"] })
            }
            None => Value::Null,
        },
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ContentBlock;

    fn answered(v: Value) -> CallToolResult {
        CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&v).unwrap())])
    }

    /// `end`'s `result` for a call answering `v`.
    fn summary(tool: &str, v: Value) -> Value {
        end(1, tool, &Ok(answered(v).into()))["result"].clone()
    }

    #[test]
    fn arguments_are_reported_small() {
        let patch: Vec<Value> = (0..500).map(|i| json!({ "op": "remove", "path": format!("/bin/c{i}") })).collect();
        let cues: Vec<Value> = (0..100).map(|i| json!({ "start": i, "text": "word" })).collect();
        let args = json!({ "patch": patch, "note": "x".repeat(300), "base_revision": 41, "pins": ["main@8.600"], "vars": { "cues": cues, "deep": { "a": { "b": { "c": 1 } } } } });
        let t = trim_args(args.as_object());
        assert_eq!(t["patch"], json!({ "ops": 500 }));
        assert_eq!((t["base_revision"].as_u64(), &t["pins"]), (Some(41), &json!(["main@8.600"])));
        assert_eq!(t["note"].as_str().unwrap().chars().count(), MAX_STRING + 1);
        assert_eq!(t["vars"]["cues"].as_array().unwrap().len(), MAX_ITEMS);
        assert_eq!(t["vars"]["deep"]["a"]["b"], json!("…"));
        assert_eq!(trim_args(None), Value::Null);
    }

    #[test]
    fn a_result_is_reduced_to_what_the_editor_shows() {
        let lint = json!({ "overlay": "title", "findings": [{ "level": "warning", "code": "a", "message": "" }, { "level": "warning", "code": "b", "message": "" }] });
        assert_eq!(summary("check_overlay", lint), json!({ "ok": true, "errors": 0, "warnings": 2 }));
        assert_eq!(summary("apply_edits", json!({ "revision": 59, "warnings": [], "ops": 2 })), json!({ "revision": 59, "ops": 2 }));
        let sample = json!({ "samples": [{ "t": 1.0, "frame": 30, "path": "/p/.kuma/samples/a.png" }], "sheet": null });
        assert_eq!(summary("sample_frames", sample), json!({ "count": 1, "paths": ["/p/.kuma/samples/a.png"], "sheet": null }));
        assert_eq!(summary("sample_frames", json!({ "job": 3, "state": "running" })), Value::Null, "still baking");
        assert_eq!(summary("get_project", json!({ "revision": 4 })), Value::Null);
    }

    /// A call that edits reports the revision its own answer names; one that reads, or edits
    /// nothing, reports none — not the project's revision when the report is sent.
    #[test]
    fn an_end_carries_the_revision_the_call_made() {
        let revision = |tool: &str, v: Value| end(1, tool, &Ok(answered(v).into()))["revision"].clone();
        assert_eq!(revision("apply_edits", json!({ "revision": 59, "ops": 1 })), json!(59));
        assert_eq!(revision("undo", json!({ "revision": 60 })), json!(60));
        assert_eq!(revision("transcribe", json!({ "id": 2, "state": "done", "result": { "revision": 61 } })), json!(61));
        assert_eq!(revision("transcribe", json!({ "id": 2, "state": "running", "result": null })), Value::Null, "still transcribing");
        assert_eq!(revision("reprobe_media", json!({ "changed": [], "revision": 61 })), Value::Null, "nothing to change");
        assert_eq!(revision("reprobe_media", json!({ "changed": [{ "id": "a" }], "revision": 62 })), json!(62));
        assert_eq!(revision("get_project", json!({ "revision": 62 })), Value::Null, "reads only");
    }

    #[test]
    fn an_end_says_whether_the_call_failed() {
        let ok = end(7, "apply_edits", &Ok(answered(json!({ "revision": 3, "ops": 1 })).into()));
        assert_eq!(ok, json!({ "phase": "end", "id": 7, "ok": true, "error": null, "result": { "revision": 3, "ops": 1 }, "revision": 3 }));
        let failed = end(8, "undo", &Err(McpError::invalid_request("nothing to undo", None)));
        assert_eq!((failed["ok"].as_bool(), failed["error"].as_str(), &failed["revision"]), (Some(false), Some("nothing to undo"), &Value::Null));
    }
}
