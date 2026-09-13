//! The project document. This is the JSON the agent and the UI edit.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::time::Fps;

pub type AssetId = String;
pub type ClipId = String;
pub type TrackId = String;

/// A kuma project. Paths are relative to the directory containing the project file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// Format version. Always 1 for now.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Human-readable project name.
    #[serde(default)]
    pub name: String,
    /// Monotonic revision, bumped on every accepted edit. Used for optimistic concurrency.
    #[serde(default)]
    pub revision: u64,
    /// Composition frame rate (rational).
    #[serde(default)]
    pub fps: Fps,
    /// Composition size in pixels. 1920×1080 landscape, 1080×1920 for Shorts.
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    /// Audio sample rate for mixing and export.
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    /// Media referenced by clips, keyed by a short stable id.
    #[serde(default)]
    pub assets: BTreeMap<AssetId, Asset>,
    /// Tracks in **bottom-to-top** compositing order: `tracks[0]` is painted first.
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// Timeline markers (annotation only; never rendered).
    #[serde(default)]
    pub markers: Vec<Marker>,
}

fn default_version() -> u32 {
    crate::PROJECT_VERSION
}
fn default_width() -> u32 {
    1920
}
fn default_height() -> u32 {
    1080
}
fn default_sample_rate() -> u32 {
    48000
}

impl Default for Project {
    fn default() -> Self {
        Project {
            version: crate::PROJECT_VERSION,
            name: String::new(),
            revision: 0,
            fps: Fps::default(),
            width: 1920,
            height: 1080,
            sample_rate: 48000,
            assets: BTreeMap::new(),
            tracks: Vec::new(),
            markers: Vec::new(),
        }
    }
}

impl Project {
    /// Timeline duration in seconds: the latest clip end across all tracks.
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.end)
            .fold(0.0, f64::max)
    }

    pub fn find_clip(&self, id: &str) -> Option<(&Track, &Clip)> {
        self.tracks
            .iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == id).map(|c| (t, c)))
    }

    pub fn find_clip_mut(&mut self, id: &str) -> Option<&mut Clip> {
        self.tracks
            .iter_mut()
            .find_map(|t| t.clips.iter_mut().find(|c| c.id == id))
    }

    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Clips sorted by start time on a track — the order the compiler walks.
    pub fn sorted_clips<'a>(&self, track: &'a Track) -> Vec<&'a Clip> {
        let mut v: Vec<&Clip> = track.clips.iter().collect();
        v.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        v
    }
}

/// A media file on disk.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    /// Path relative to the project directory (or absolute).
    pub path: String,
    #[serde(default)]
    pub kind: AssetKind,
    /// Optional low-resolution proxy used for preview instead of `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// Probed metadata, filled in by `probe_asset`. Informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<AssetMeta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    #[default]
    Video,
    Audio,
    Image,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Fps>,
    #[serde(default)]
    pub has_audio: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Video clips (with their embedded audio unless muted).
    #[default]
    Video,
    /// Audio-only clips.
    Audio,
    /// HTML overlay instances. Rendered as live DOM in preview, baked to alpha video on export.
    Overlay,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub id: TrackId,
    #[serde(default)]
    pub kind: TrackKind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Transitions between adjacent clips on this track.
    #[serde(default)]
    pub transitions: Vec<Transition>,
}

/// One item on the timeline.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: ClipId,
    /// Timeline start, seconds.
    pub start: f64,
    /// Timeline end, seconds, **exclusive**.
    pub end: f64,
    /// What this clip shows.
    pub src: ClipSource,
    /// Playback speed. 1.0 = normal, 0.5 = half speed, -1.0 = reverse.
    /// For overlays this is applied at bake time (time-remapped seek); for media via MLT timewarp.
    #[serde(default = "one")]
    pub speed: f64,
    /// Linear audio gain. 1.0 = 0 dB. Ignored for silent sources.
    #[serde(default = "one")]
    pub volume: f64,
    /// Mute this clip's audio.
    #[serde(default)]
    pub muted: bool,
    /// Effects applied in order.
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Free-form label shown in the UI.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

fn one() -> f64 {
    1.0
}

impl Clip {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// Where a clip's pixels/samples come from. Exactly one variant.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ClipSource {
    /// A media asset.
    Asset {
        asset: AssetId,
        /// Offset into the source media, seconds. Default 0.
        #[serde(default, rename = "in")]
        source_in: f64,
    },
    /// An HTML overlay template instance.
    Overlay {
        /// Template id — a directory name under `overlays/`.
        overlay: String,
        /// Variable overrides, validated against the template manifest.
        #[serde(default)]
        vars: Map<String, Value>,
    },
    /// A solid colour.
    Color {
        /// CSS-style colour, e.g. `#000000` or `#ff4f00cc`.
        color: String,
    },
}

/// A keyframed transform sample. Fields not set fall back to the effect's static values.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransformKeyframe {
    /// Time relative to clip start, seconds.
    pub t: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
}

/// Blend modes, mapped by the compiler onto the compositor's modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

/// An effect on a clip. `type` selects the variant.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    /// Position / scale / rotation / opacity. `x`/`y` are pixel offsets from the centred
    /// position; `scale` multiplies the composition size; `rotation` in degrees.
    Transform {
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
        #[serde(default = "one")]
        scale: f64,
        #[serde(default)]
        rotation: f64,
        #[serde(default = "one")]
        opacity: f64,
        /// Optional keyframes; when present they override the static values over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keyframes: Vec<TransformKeyframe>,
    },
    /// Crop as fractions of the frame (0..1) from each edge, with optional rounded corners.
    Crop {
        #[serde(default)]
        left: f64,
        #[serde(default)]
        right: f64,
        #[serde(default)]
        top: f64,
        #[serde(default)]
        bottom: f64,
        /// Corner radius as a fraction of the shorter side (0..1).
        #[serde(default)]
        radius: f64,
    },
    /// Blend mode used when compositing this clip over the tracks below.
    Blend { mode: BlendMode },
    /// Gaussian blur, radius in pixels.
    Blur { radius: f64 },
    /// Basic colour correction. All default to neutral.
    Color {
        #[serde(default)]
        brightness: f64,
        #[serde(default = "one")]
        contrast: f64,
        #[serde(default = "one")]
        saturation: f64,
        #[serde(default = "one")]
        gamma: f64,
    },
    /// Opacity fade in/out, durations in seconds.
    Fade {
        #[serde(default, rename = "in")]
        fade_in: f64,
        #[serde(default, rename = "out")]
        fade_out: f64,
    },
    /// Keyframed audio gain (linear). `t` relative to clip start.
    Volume { keyframes: Vec<VolumeKeyframe> },
    /// Escape hatch: any MLT filter by service name with raw properties.
    Mlt {
        service: String,
        #[serde(default)]
        properties: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeKeyframe {
    pub t: f64,
    pub gain: f64,
}

/// A transition between two adjacent clips on the same track. The cut point is the
/// boundary between them; the transition spans `duration` seconds centred on it, so the
/// `to` clip must have `duration/2` of source handle before its `in` point.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub from: ClipId,
    pub to: ClipId,
    #[serde(default)]
    pub kind: TransitionKind,
    pub duration: f64,
    /// Wipe pattern: a luma image name (e.g. `wipe_left`) or a path. Only for `wipe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luma: Option<String>,
    /// Softness of the wipe edge, 0..1.
    #[serde(default)]
    pub softness: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    #[default]
    Dissolve,
    Wipe,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub t: f64,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_project_round_trips() {
        let json = r#"{
          "version": 1, "name": "demo",
          "fps": {"num": 30000, "den": 1001}, "width": 1920, "height": 1080,
          "assets": { "a1": { "path": "media/screen.mp4", "kind": "video" } },
          "tracks": [
            { "id": "v1", "kind": "video", "clips": [
              { "id": "c1", "start": 0.0, "end": 12.5, "src": { "asset": "a1", "in": 3.2 },
                "effects": [
                  { "type": "crop", "left": 0.05, "right": 0.05 },
                  { "type": "transform", "keyframes": [
                    { "t": 0.0, "scale": 1.0 }, { "t": 3.0, "scale": 1.4, "x": -120 } ] } ] } ] },
            { "id": "o1", "kind": "overlay", "clips": [
              { "id": "o_c1", "start": 2.0, "end": 8.0,
                "src": { "overlay": "lower-third", "vars": { "title": "kuma" } },
                "speed": 0.5,
                "effects": [ { "type": "blend", "mode": "screen" }, { "type": "blur", "radius": 2 } ] } ] }
          ]
        }"#;
        let p: Project = serde_json::from_str(json).expect("parse");
        assert_eq!(p.tracks.len(), 2);
        assert!(matches!(p.tracks[1].clips[0].src, ClipSource::Overlay { .. }));
        assert_eq!(p.duration(), 12.5);
        let back = serde_json::to_string(&p).unwrap();
        let p2: Project = serde_json::from_str(&back).unwrap();
        assert_eq!(p2.tracks[0].clips[0].effects.len(), 2);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = r#"{ "tracks": [ { "id": "v1", "clipz": [] } ] }"#;
        assert!(serde_json::from_str::<Project>(json).is_err());
    }
}
