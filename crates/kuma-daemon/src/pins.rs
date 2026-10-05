//! Pins: the person's notes on frames, for the agent to answer.
//!
//! A pin is not a model concept. It is a sequence **marker** with a label — the comment the
//! person left at that moment — seen from the agent's side: listed with an id an edit can
//! name, and with whether one has answered it. Markers have no id and the format is not
//! changed for this, so the id is derived from where the marker is (`<sequence>@<t>`), and
//! whether it is done is session state here, never written into the document.

use std::collections::HashMap;

use kuma_core::{BinId, Project, Sequence};
use serde::Serialize;

/// A pin's id: its sequence and its time to the millisecond, `master@8.600`.
pub fn pin_id(sequence: &str, t: f64) -> String {
    format!("{sequence}@{t:.3}")
}

#[derive(Debug, Clone, Serialize)]
pub struct Pin {
    pub id: String,
    pub sequence: BinId,
    /// 1-based, among this sequence's pins in time order: what the editor numbers it.
    pub index: usize,
    pub t: f64,
    pub label: String,
    pub color: Option<String>,
    /// The revision that answered it, or the one current when it was ticked by hand.
    pub done: Option<u64>,
    /// `agent`, `user` or `other`: who answered it (a tick is the user's).
    pub done_by: Option<String>,
}

/// How a pin was answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub revision: u64,
    pub by: String,
}

/// Which pins are done, by id. The latest answer wins: an edit naming a pin, a tick, or an
/// untick, which forgets both.
pub type Answers = HashMap<String, Done>;

/// Every pin in `p`: the master's first, then the other sequences' in bin order.
pub fn list(p: &Project, answers: &Answers) -> Vec<Pin> {
    let master = p.master_seq().map(|s| (&p.master, s));
    let others = p.sequences().filter(|(id, _)| **id != p.master);
    master.into_iter().chain(others).flat_map(|(id, s)| of_sequence(id, s, answers)).collect()
}

fn of_sequence(id: &BinId, s: &Sequence, answers: &Answers) -> Vec<Pin> {
    let mut marks: Vec<_> = s.markers.iter().filter(|m| !m.label.trim().is_empty()).collect();
    marks.sort_by(|a, b| a.t.total_cmp(&b.t));
    marks
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let pid = pin_id(id, m.t);
            let done = answers.get(&pid);
            Pin {
                index: i + 1,
                sequence: id.clone(),
                t: m.t,
                label: m.label.clone(),
                color: m.color.clone(),
                done: done.map(|d| d.revision),
                done_by: done.map(|d| d.by.clone()),
                id: pid,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_pin_is_a_labelled_marker_named_by_where_it_is() {
        assert_eq!(pin_id("master", 8.6), "master@8.600");
        assert_eq!(pin_id("intro", 0.0), "intro@0.000");
        let p: Project = serde_json::from_value(json!({
            "master": "main",
            "bin": {
                "a": { "source": { "tracks": [], "markers": [{ "t": 1.0, "label": "nested" }] } },
                "main": { "source": { "tracks": [], "markers": [
                    { "t": 8.6, "label": "HUD fights the subtitles", "color": "#f00" },
                    { "t": 2.0, "label": "" },
                    { "t": 1.25, "label": "too fast" }
                ] } }
            }
        }))
        .unwrap();
        let answers: Answers = [("main@8.600".to_string(), Done { revision: 3, by: "agent".into() })].into();
        let pins = list(&p, &answers);
        let got: Vec<(&str, usize, Option<u64>)> = pins.iter().map(|p| (p.id.as_str(), p.index, p.done)).collect();
        assert_eq!(got, [("main@1.250", 1, None), ("main@8.600", 2, Some(3)), ("a@1.000", 1, None)], "the master first, an empty label is no pin");
        assert_eq!(pins[1].done_by.as_deref(), Some("agent"));
        assert_eq!(pins[1].color.as_deref(), Some("#f00"));
    }
}
