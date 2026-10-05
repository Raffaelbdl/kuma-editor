//! What this server tells the daemon about each tool call, so the editor can show what the
//! agent is doing and not only the patches it lands (`POST /api/activity`).
//!
//! A report is small by construction: the arguments trimmed (a patch is its op count, long
//! strings and lists are cut), and the result reduced to the one thing the editor shows
//! for that tool. Everything here is pure; sending is `Client::report`, best effort.

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

/// The start of a call.
pub fn start(tool: &str, client: &str, args: &Value) -> Value {
    json!({ "phase": "start", "tool": tool, "client": client, "args": trim_args(args) })
}

/// The end of call `id`, from its response.
pub fn end(id: u64, tool: &str, response: &Result<CallToolResponse, McpError>) -> Value {
    let (ok, error, result) = match response {
        Ok(CallToolResponse::Complete(r)) if r.is_error == Some(true) => (false, first_text(r), Value::Null),
        Ok(CallToolResponse::Complete(r)) => (true, None, summary(tool, r)),
        Ok(_) => (true, None, Value::Null),
        Err(e) => (false, Some(e.message.to_string()), Value::Null),
    };
    json!({ "phase": "end", "id": id, "ok": ok, "error": error, "result": result })
}

/// A call's arguments, small: `patch` as `{"ops": n}` (its `note`, `base_revision` and
/// `pins` are kept beside it), and every string, list and object capped.
pub fn trim_args(args: &Value) -> Value {
    let mut out = args.clone();
    if let Some(ops) = out.get("patch").and_then(Value::as_array).map(Vec::len) {
        out["patch"] = json!({ "ops": ops });
    }
    cap(&out, 0)
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

fn first_text(r: &CallToolResult) -> Option<String> {
    r.content.iter().find_map(|c| c.as_text().map(|t| t.text.clone()))
}

/// What the editor shows of a finished call: a lint's verdict, an edit's revision, the
/// frames a sample made. Read off the JSON the tool answered; `null` for every other tool,
/// and for a sample still baking.
pub fn summary(tool: &str, r: &CallToolResult) -> Value {
    let Some(v) = first_text(r).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return Value::Null };
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

    #[test]
    fn arguments_are_reported_small() {
        let patch: Vec<Value> = (0..500).map(|i| json!({ "op": "remove", "path": format!("/bin/c{i}") })).collect();
        let cues: Vec<Value> = (0..100).map(|i| json!({ "start": i, "text": "word" })).collect();
        let args = json!({ "patch": patch, "note": "x".repeat(300), "base_revision": 41, "pins": ["main@8.600"], "vars": { "cues": cues, "deep": { "a": { "b": { "c": 1 } } } } });
        let t = trim_args(&args);
        assert_eq!(t["patch"], json!({ "ops": 500 }));
        assert_eq!((t["base_revision"].as_u64(), &t["pins"]), (Some(41), &json!(["main@8.600"])));
        assert_eq!(t["note"].as_str().unwrap().chars().count(), MAX_STRING + 1);
        assert_eq!(t["vars"]["cues"].as_array().unwrap().len(), MAX_ITEMS);
        assert_eq!(t["vars"]["deep"]["a"]["b"], json!("…"));
        assert_eq!(trim_args(&Value::Null), Value::Null);
    }

    #[test]
    fn a_result_is_reduced_to_what_the_editor_shows() {
        let lint = answered(json!({ "overlay": "title", "findings": [{ "level": "warning", "code": "a", "message": "" }, { "level": "warning", "code": "b", "message": "" }] }));
        assert_eq!(summary("check_overlay", &lint), json!({ "ok": true, "errors": 0, "warnings": 2 }));
        assert_eq!(summary("apply_edits", &answered(json!({ "revision": 59, "warnings": [], "ops": 2 }))), json!({ "revision": 59, "ops": 2 }));
        let sample = answered(json!({ "samples": [{ "t": 1.0, "frame": 30, "path": "/p/.kuma/samples/a.png" }], "sheet": null }));
        assert_eq!(summary("sample_frames", &sample), json!({ "count": 1, "paths": ["/p/.kuma/samples/a.png"], "sheet": null }));
        assert_eq!(summary("sample_frames", &answered(json!({ "job": 3, "state": "running" }))), Value::Null, "still baking");
        assert_eq!(summary("get_project", &answered(json!({}))), Value::Null);
    }

    #[test]
    fn an_end_says_whether_the_call_failed() {
        let ok = end(7, "apply_edits", &Ok(answered(json!({ "revision": 3, "ops": 1 })).into()));
        assert_eq!(ok, json!({ "phase": "end", "id": 7, "ok": true, "error": null, "result": { "revision": 3, "ops": 1 } }));
        let failed = end(8, "undo", &Err(McpError::invalid_request("nothing to undo", None)));
        assert_eq!((failed["ok"].as_bool(), failed["error"].as_str()), (Some(false), Some("nothing to undo")));
    }
}
