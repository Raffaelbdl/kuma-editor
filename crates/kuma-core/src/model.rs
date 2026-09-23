//! The project document. This is the JSON the agent and the UI edit.

use std::collections::{BTreeMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::time::Fps;

pub type BinId = String;
pub type ClipId = String;
pub type TrackId = String;

/// A kuma project. Paths are relative to the directory containing the project file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// Format version. Always 1 for now; bumping it means writing an upgrade step for
    /// documents already on disk.
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
    /// Audio sample rate of the export (`ar` on the melt consumer). The preview engine
    /// mixes at 48 kHz regardless.
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    /// Everything this project can place on a track — media files, overlay instances,
    /// colours and *sequences* — keyed by a short stable id. Clips reference an entry;
    /// they never carry a source of their own.
    #[serde(default)]
    pub bin: BTreeMap<BinId, BinItem>,
    /// The bin entry holding the master timeline. It must be a [`BinSource::Sequence`];
    /// it is what `render` renders and what [`Project::duration`] measures. Every other
    /// sequence is reached by being placed on it, directly or through another sequence.
    #[serde(default = "default_master")]
    pub master: BinId,
}

fn default_version() -> u32 {
    crate::PROJECT_VERSION
}
fn default_master() -> BinId {
    "main".into()
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
            bin: BTreeMap::new(),
            master: default_master(),
        }
    }
}

impl Project {
    /// A fresh project with the three default tracks (video, overlays, audio). Used by
    /// `kuma init` and by the daemon when the project file does not exist yet.
    pub fn new_default(name: impl Into<String>, width: u32, height: u32, fps: Fps) -> Self {
        let track = |id: &str, kind: TrackKind, name: &str| Track {
            id: id.into(),
            kind,
            name: name.into(),
            muted: false,
            hidden: false,
            clips: Vec::new(),
            transitions: Vec::new(),
        };
        let main = BinItem {
            name: "Main".into(),
            source: BinSource::Sequence(Sequence {
                tracks: vec![
                    track("v1", TrackKind::Video, "Video"),
                    track("o1", TrackKind::Overlay, "Overlays"),
                    track("a1", TrackKind::Audio, "Audio"),
                ],
                markers: Vec::new(),
            }),
            meta: None,
            parent: None,
            order: None,
        };
        Project {
            name: name.into(),
            width,
            height,
            fps,
            bin: [(default_master(), main)].into(),
            ..Project::default()
        }
    }

    /// Project name implied by a `<name>.kuma.json` path.
    pub fn name_from_path(path: &std::path::Path) -> String {
        path.file_stem().map(|s| s.to_string_lossy().trim_end_matches(".kuma").to_string()).unwrap_or_default()
    }

    /// The timeline held by a bin entry, if that entry is a sequence.
    pub fn seq(&self, id: &str) -> Option<&Sequence> {
        match self.bin.get(id).map(|i| &i.source) {
            Some(BinSource::Sequence(s)) => Some(s),
            _ => None,
        }
    }

    /// The master timeline. `None` only for a document the validator would reject.
    pub fn master_seq(&self) -> Option<&Sequence> {
        self.seq(&self.master)
    }

    /// Every sequence in the bin, id and all, in bin order.
    pub fn sequences(&self) -> impl Iterator<Item = (&BinId, &Sequence)> {
        self.bin.iter().filter_map(|(id, item)| match &item.source {
            BinSource::Sequence(s) => Some((id, s)),
            _ => None,
        })
    }

    /// The sequence a job should treat as its root: the one asked for, or the master.
    /// Every entry point that takes an optional sequence resolves it here, so they cannot
    /// drift on either the default or the wording of the refusal.
    pub fn sequence_or_master(&self, id: Option<&str>) -> Result<&BinId, String> {
        let wanted = id.unwrap_or(&self.master);
        // Borrow the id out of the bin so the caller gets one that outlives its argument.
        match self.bin.get_key_value(wanted) {
            Some((id, item)) if item.is_sequence() => Ok(id),
            _ => Err(format!("`{wanted}` is not a sequence in this project")),
        }
    }

    /// Every clip in the project, with the sequence it lives on. Cheap enough to walk:
    /// jobs that touch "every overlay clip" use this rather than knowing about nesting.
    pub fn all_clips(&self) -> impl Iterator<Item = (&BinId, &Track, &Clip)> {
        self.sequences().flat_map(|(id, s)| s.tracks.iter().flat_map(move |t| t.clips.iter().map(move |c| (id, t, c))))
    }

    /// A sequence's own duration in seconds: the latest clip end across its tracks. An id
    /// that is not a sequence measures 0.
    pub fn seq_duration(&self, id: &str) -> f64 {
        self.seq(id).map(Sequence::duration).unwrap_or(0.0)
    }

    /// Master timeline duration in seconds — what `render` produces.
    pub fn duration(&self) -> f64 {
        self.seq_duration(&self.master)
    }

    /// Find a clip by id, anywhere in the project. Clip ids are unique across every
    /// sequence, which is what lets the daemon address one by id alone.
    pub fn find_clip(&self, id: &str) -> Option<(&BinId, &Track, &Clip)> {
        self.all_clips().find(|(_, _, c)| c.id == id)
    }

    /// Sequences reachable from `root`, **innermost first**: the order the compiler must
    /// emit tractors in, so an outer sequence only ever references one already written.
    /// `Err` carries the reference chain that closes a cycle, ending on the repeat.
    pub fn compile_order(&self, root: &str) -> Result<Vec<BinId>, Vec<BinId>> {
        let mut walk = SequenceWalk::default();
        walk.visit(self, root)?;
        Ok(walk.order)
    }

    /// Refuse a bin in which any sequence contains itself, returning the ring that closes.
    /// One walk for the whole bin rather than one per sequence: the visited set is shared,
    /// so an entry reached from several roots is expanded once.
    pub fn check_cycles(&self) -> Result<(), Vec<BinId>> {
        let mut walk = SequenceWalk::default();
        self.sequences().try_for_each(|(id, _)| walk.visit(self, id))
    }

    /// Whether a clip contributes audible sound: what decides if its track needs an audio
    /// mix. Stated once here because the compiler asks the same question of the same clip,
    /// and the two answers have to agree or a render comes out silent.
    pub fn clip_has_audio(&self, clip: &Clip) -> bool {
        !clip.muted
            && match self.item(clip) {
                Some(item) if item.is_sequence() => self.seq_has_audio(&clip.bin),
                Some(item) => item.is_timed_media(),
                None => false,
            }
    }

    /// What a clip on `track` can show and sound — which is what an effect on it has to act
    /// on to do anything ([`Effect::acts_on`]). `None` for a clip with neither, or whose
    /// entry does not resolve.
    ///
    /// A capability, not a state: muting the clip or its track changes nothing here, so an
    /// effect stays offered, and valid, across a mute. Picture is the track's to allow (an
    /// audio track draws none) and the source's to have; sound is the source's alone, the
    /// probe's word where it has spoken and "yes" for a file it has not, as the daemon
    /// assumes when it transcribes one.
    pub fn clip_streams(&self, track: &Track, clip: &Clip) -> Option<Streams> {
        let item = self.item(clip)?;
        let (video, audio) = match &item.source {
            BinSource::File { kind: MediaKind::Image, .. } => (true, false),
            BinSource::File { kind, .. } => (*kind != MediaKind::Audio, item.meta.as_ref().is_none_or(|m| m.has_audio)),
            BinSource::Sequence(_) => (true, self.seq_has_audio(&clip.bin)),
            BinSource::Overlay { .. } | BinSource::Color { .. } => (true, false),
            BinSource::Folder { .. } => (false, false),
        };
        Streams::of(video && track.kind != TrackKind::Audio, audio)
    }

    /// Whether a track sounds: unmuted, with a clip that does.
    pub fn track_has_audio(&self, track: &Track) -> bool {
        !track.muted && track.clips.iter().any(|c| self.clip_has_audio(c))
    }

    /// Whether a sequence carries any audible sound, following nested sequences. Cycle-safe.
    pub fn seq_has_audio(&self, id: &str) -> bool {
        fn walk<'a>(p: &'a Project, id: &'a str, seen: &mut HashSet<&'a str>) -> bool {
            if !seen.insert(id) {
                return false;
            }
            let Some(seq) = p.seq(id) else { return false };
            seq.tracks.iter().filter(|t| !t.muted).flat_map(|t| t.clips.iter()).any(|c| match p.item(c) {
                _ if c.muted => false,
                Some(item) if item.is_sequence() => p.bin.get_key_value(&c.bin).is_some_and(|(id, _)| walk(p, id, seen)),
                Some(item) => item.is_timed_media(),
                None => false,
            })
        }
        walk(self, id, &mut HashSet::new())
    }

    /// The bin entry a clip plays, if it resolves.
    pub fn item(&self, clip: &Clip) -> Option<&BinItem> {
        self.bin.get(&clip.bin)
    }

    /// `base` if it is free, else `base-2`, `base-3`… — the shape the editor names bin
    /// entries in, so a minted id reads like one someone typed.
    pub fn free_bin_id(&self, base: &str) -> BinId {
        std::iter::once(base.to_string())
            .chain((2..).map(|n| format!("{base}-{n}")))
            .find(|id| !self.bin.contains_key(id))
            .unwrap_or_default()
    }

    /// Lowest free `<prefix><n>` track or clip id — one namespace, the whole project.
    pub fn free_track_or_clip_id(&self, prefix: &str) -> String {
        let taken: HashSet<&str> = self.sequences().flat_map(|(_, s)| s.tracks.iter().flat_map(|t| std::iter::once(t.id.as_str()).chain(t.clips.iter().map(|c| c.id.as_str())))).collect();
        (1..).map(|n| format!("{prefix}{n}")).find(|id| !taken.contains(id.as_str())).unwrap_or_default()
    }

    /// Entries listed directly under `parent` (`None` for the top level), in browser
    /// order: by `order` where set, then by id.
    pub fn children(&self, parent: Option<&str>) -> Vec<(&BinId, &BinItem)> {
        let mut v: Vec<_> = self.bin.iter().filter(|(_, i)| i.parent.as_deref() == parent).collect();
        v.sort_by_key(|(id, i)| (i.order.unwrap_or(u32::MAX), *id));
        v
    }
}

/// A depth-first walk of the sequence graph, shared by `compile_order` (one root, and the
/// order is the answer) and `check_cycles` (every root, and only the failure is).
#[derive(Default)]
struct SequenceWalk {
    /// Reachable sequences, innermost first.
    order: Vec<BinId>,
    done: HashSet<BinId>,
    /// The chain currently being expanded; an id already on it closes a ring.
    path: Vec<BinId>,
}

impl SequenceWalk {
    fn visit(&mut self, p: &Project, id: &str) -> Result<(), Vec<BinId>> {
        if self.done.contains(id) {
            return Ok(());
        }
        if let Some(at) = self.path.iter().position(|p| p == id) {
            // The ring itself, not the walk that found it: from the repeated id round to it.
            return Err(self.path[at..].iter().cloned().chain([id.to_string()]).collect());
        }
        let Some(seq) = p.seq(id) else { return Ok(()) };
        self.path.push(id.to_string());
        // Visiting a clip that plays something other than a sequence is a no-op, so the
        // walk does not need to know which clips are which.
        for clip in seq.clips() {
            self.visit(p, &clip.bin)?;
        }
        self.path.pop();
        self.done.insert(id.to_string());
        self.order.push(id.to_string());
        Ok(())
    }
}

impl Track {
    /// Clips with their index in `clips`, sorted by start time — the order the
    /// validator and the compiler walk.
    pub fn sorted_clips(&self) -> Vec<(usize, &Clip)> {
        let mut v: Vec<(usize, &Clip)> = self.clips.iter().enumerate().collect();
        v.sort_by(|a, b| a.1.start.partial_cmp(&b.1.start).unwrap_or(std::cmp::Ordering::Equal));
        v
    }
}

/// Track and clip ids double as XML ids and URL segments: a letter or underscore,
/// then letters, digits, `_` or `-`.
pub fn is_valid_id(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// One entry in the project bin: something that can occupy time on a track. A clip is a
/// placement of one of these, so everything that describes *what* is shown lives here and
/// everything that describes *when* lives on the clip.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinItem {
    /// Display name in the library browser. Falls back to the id when empty.
    #[serde(default)]
    pub name: String,
    pub source: BinSource,
    /// Probed metadata for file entries, filled in by `probe_media`. Informational.
    #[serde(default)]
    pub meta: Option<MediaMeta>,
    /// The entry this one is listed under in the browser: a transcript under the media it
    /// came from, later a folder. Organisation only — nothing about rendering reads it.
    /// Identity stays the id, so moving an entry is one field and no clip changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<BinId>,
    /// Position among its siblings in the browser. Entries with an order come first, in
    /// that order; the rest follow by id, so a new entry lands at the bottom. Organisation
    /// only, like `parent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
}

/// What a bin entry actually is. Exactly one variant.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum BinSource {
    /// A media file on disk: video, audio or a still image.
    File {
        /// Path relative to the project directory (or absolute).
        path: String,
        #[serde(default)]
        kind: MediaKind,
        /// Optional low-resolution stand-in used instead of `path` when compiling for
        /// preview (never for renders or samples).
        #[serde(default)]
        proxy: Option<String>,
    },
    /// A configured HTML overlay: a template id plus the variables for this instance.
    /// Two entries of the same template with different vars are two separate items.
    Overlay {
        /// Template id, resolved against the template libraries.
        template: String,
        /// Variable overrides, validated against the template manifest.
        #[serde(default)]
        vars: Map<String, Value>,
    },
    /// A solid colour.
    Color {
        /// CSS-style colour, e.g. `#000000` or `#ff4f00cc`.
        color: String,
    },
    /// A timeline of its own, placeable on another timeline. The project's `master` is
    /// one of these; so is every part edited separately and juxtaposed in the final cut.
    Sequence(Sequence),
    /// A folder: an entry that exists to be a `parent`. Never placeable. The field is
    /// what tells it apart from the other sources; its value is always `true`.
    Folder { folder: bool },
}

/// A timeline: tracks and their markers. Held by a [`BinSource::Sequence`] entry rather
/// than by the project, so the master timeline and a nested one are the same thing and
/// every clip operation works on both.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    /// Tracks in **bottom-to-top** compositing order: `tracks[0]` is painted first.
    /// Required (not defaulted) so an empty object cannot deserialize as a sequence.
    pub tracks: Vec<Track>,
    /// Timeline markers (annotation only; never rendered).
    #[serde(default)]
    pub markers: Vec<Marker>,
}

impl Sequence {
    /// Every clip on this timeline, in track order. The one place a timeline is walked,
    /// so a new traversal does not spell the nesting out again.
    pub fn clips(&self) -> impl Iterator<Item = &Clip> {
        self.tracks.iter().flat_map(|t| t.clips.iter())
    }

    /// Duration in seconds: the latest clip end across the sequence's tracks.
    pub fn duration(&self) -> f64 {
        self.clips().map(|c| c.end).fold(0.0, f64::max)
    }
}

impl BinItem {
    /// True when the source carries its own timeline to seek into, so a clip's `in` is
    /// meaningful. Stills, colours and overlays all start at their own zero and are
    /// generated to fit the clip instead; a sequence has a timeline like a media file.
    pub fn is_timed_media(&self) -> bool {
        match &self.source {
            BinSource::File { kind, .. } => *kind != MediaKind::Image,
            BinSource::Sequence(_) => true,
            _ => false,
        }
    }

    pub fn is_overlay(&self) -> bool {
        matches!(self.source, BinSource::Overlay { .. })
    }

    pub fn is_sequence(&self) -> bool {
        matches!(self.source, BinSource::Sequence(_))
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.source, BinSource::Folder { .. })
    }

    /// A media file the probe found no video in. It renders nothing, so on a video track
    /// it occupies a lane and shows the tracks below it.
    pub fn is_audio_only(&self) -> bool {
        matches!(&self.source, BinSource::File { kind, .. } if *kind == MediaKind::Audio)
    }

    /// What the browser shows for this entry.
    pub fn display_name<'a>(&'a self, id: &'a str) -> &'a str {
        if self.name.is_empty() {
            id
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    #[default]
    Video,
    Audio,
    Image,
}

/// Informational, written by the probe, and the probe's output evolves: unknown keys are
/// tolerated so a project file written by another version still loads.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MediaMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Fps>,
    /// The source's frames are not evenly spaced, so `fps` is their average rather than a
    /// grid. Nothing renders differently: times are seconds, sampled at the project rate.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub variable_fps: bool,
    #[serde(default)]
    pub has_audio: bool,
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

/// One placement of a bin entry on the timeline.
///
/// Every field of the editing model is always serialized, so a JSON Patch `replace` on
/// any documented path succeeds (only probed `MediaMeta` and manifest specs skip nulls).
/// The one exception here is `group`, absent when empty so that an ungrouped file is the
/// file it was: write it with `add`, which works whether or not it is there.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: ClipId,
    /// Timeline start, seconds.
    pub start: f64,
    /// Timeline end, seconds, **exclusive**.
    pub end: f64,
    /// The bin entry this clip plays.
    pub bin: BinId,
    /// Offset into the source media, seconds. File entries only: a still, colour or
    /// overlay is generated to fit the clip and has nothing to offset into.
    #[serde(default, rename = "in")]
    pub source_in: f64,
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
    #[serde(default)]
    pub label: String,
    /// The group this clip belongs to, or empty for none. An **editing relation** only:
    /// clips sharing one are selected and moved together, and the compiler never reads it.
    /// The members are whichever clips carry the id, so a delete leaves nothing dangling
    /// and a group belongs to one sequence — which the validator checks.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
}

fn one() -> f64 {
    1.0
}

impl Clip {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// How a keyframe reaches the **next** one. A segment's shape is decided by the keyframe
/// that opens it, so the last one's `ease` never means anything; absent is `linear`. These
/// are the shapes the render engine interpolates natively, and how it spells them is the
/// compiler's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    /// A straight line to the next key.
    #[default]
    Linear,
    /// Hold this value until the next key, then jump.
    Hold,
    /// A Catmull-Rom curve through the neighbouring keys.
    Smooth,
}

impl Ease {
    /// Serialization skips the default, so a project that does not ease is unchanged by
    /// this field existing.
    pub fn is_linear(&self) -> bool {
        *self == Ease::Linear
    }
}

/// One keyframe of any effect: when, how it reaches the next one, and whichever of the
/// effect's numeric parameters it sets, under the names the static fields have —
/// `{ "t": 1.5, "x": 120, "scale": 1.2 }`.
///
/// A parameter a key omits means **the effect's static value**, not "unchanged", which is
/// what lets a hand-written key set one parameter and why a key the editor writes carries
/// every animated one. The validator checks the names against [`Effect::animatable`], so
/// this struct cannot `deny_unknown_fields`: the parameters *are* its unknown fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Keyframe {
    /// Time relative to clip start, seconds.
    pub t: f64,
    /// How this key reaches the next one.
    #[serde(default, skip_serializing_if = "Ease::is_linear")]
    pub ease: Ease,
    /// The parameters this key sets.
    #[serde(flatten)]
    pub values: BTreeMap<String, f64>,
}

impl Keyframe {
    pub fn get(&self, name: &str) -> Option<f64> {
        self.values.get(name).copied()
    }

    /// The parameter, or `stat` — the effect's static value — when this key leaves it out.
    pub fn or(&self, name: &str, stat: f64) -> f64 {
        self.get(name).unwrap_or(stat)
    }
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
        #[serde(default)]
        keyframes: Vec<Keyframe>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keyframes: Vec<Keyframe>,
    },
    /// Blend mode used when compositing this clip over the tracks below.
    Blend { mode: BlendMode },
    /// Gaussian blur, radius in pixels.
    Blur {
        radius: f64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keyframes: Vec<Keyframe>,
    },
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keyframes: Vec<Keyframe>,
    },
    /// Picture fade in/out (opacity), durations in seconds. The sound's is `audio_fade`.
    Fade {
        #[serde(default, rename = "in")]
        fade_in: f64,
        #[serde(default, rename = "out")]
        fade_out: f64,
    },
    /// Sound fade in/out: the gain rises from silence over `in` seconds at the clip's start
    /// and falls back to it over `out` at its end. A kind of its own rather than a switch on
    /// `fade`, so a picture fade already in a finished video goes on leaving its sound alone,
    /// and so the two can take different lengths on one clip.
    AudioFade {
        #[serde(default, rename = "in")]
        fade_in: f64,
        #[serde(default, rename = "out")]
        fade_out: f64,
    },
    /// Audio gain (linear): 1 is the clip as recorded. It multiplies the clip's own
    /// `volume`, which is the placement's fader; this is a gain stage that can move.
    Volume {
        #[serde(default = "one")]
        gain: f64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keyframes: Vec<Keyframe>,
    },
    /// Escape hatch: any MLT filter by service name with raw properties.
    Mlt {
        service: String,
        #[serde(default)]
        properties: BTreeMap<String, String>,
    },
}

/// Picture, sound, or both: what an effect acts on, and what a compiled graph carries. A
/// render is always both; the preview compiles the picture and the sound as two graphs,
/// so that an edit to one reloads only that one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Streams {
    #[default]
    Both,
    Video,
    Audio,
}

impl Streams {
    /// The streams named by two flags, or `None` for neither.
    pub fn of(video: bool, audio: bool) -> Option<Streams> {
        match (video, audio) {
            (true, true) => Some(Streams::Both),
            (true, false) => Some(Streams::Video),
            (false, true) => Some(Streams::Audio),
            (false, false) => None,
        }
    }
    pub fn video(self) -> bool {
        self != Streams::Audio
    }
    pub fn audio(self) -> bool {
        self != Streams::Video
    }
    /// Whether the two share a stream.
    pub fn meets(self, other: Streams) -> bool {
        (self.video() && other.video()) || (self.audio() && other.audio())
    }
}

impl Effect {
    /// What this effect acts on. The escape hatch's service is opaque, so it is both: an
    /// image filter nobody asks for an image from costs nothing.
    pub fn streams(&self) -> Streams {
        match self {
            Effect::Volume { .. } | Effect::AudioFade { .. } => Streams::Audio,
            Effect::Mlt { .. } => Streams::Both,
            Effect::Transform { .. } | Effect::Crop { .. } | Effect::Blur { .. } | Effect::Color { .. } | Effect::Fade { .. } | Effect::Blend { .. } => {
                Streams::Video
            }
        }
    }

    /// Whether this effect does anything to a clip carrying `clip` — see
    /// [`Project::clip_streams`]. The editor offers only the kinds that do, and the
    /// validator warns about one already placed that does not.
    pub fn acts_on(&self, clip: Streams) -> bool {
        self.streams().meets(clip)
    }

    /// The parameters a keyframe of this kind may set, under the names its static fields
    /// have. One table, so the validator, the compiler and the editor cannot disagree.
    ///
    /// Every kind with a quantity to move is here, static until it is given keys. The
    /// three that are not have nothing a key could mean: a fade's numbers are lengths of
    /// time, so it is an animation already; a blend mode is a choice, and changing it
    /// mid-clip is a cut; and `mlt` properties are strings MLT reads, where an animation
    /// string can be written directly.
    pub fn animatable(&self) -> &'static [&'static str] {
        match self {
            Effect::Transform { .. } => &["x", "y", "scale", "rotation", "opacity"],
            Effect::Crop { .. } => &["left", "right", "top", "bottom", "radius"],
            Effect::Blur { .. } => &["radius"],
            Effect::Color { .. } => &["brightness", "contrast", "saturation", "gamma"],
            Effect::Volume { .. } => &["gain"],
            Effect::Blend { .. } | Effect::Fade { .. } | Effect::AudioFade { .. } | Effect::Mlt { .. } => &[],
        }
    }

    /// This effect's keyframes; empty for a kind that has none, or has none set.
    pub fn keyframes(&self) -> &[Keyframe] {
        match self {
            Effect::Transform { keyframes, .. }
            | Effect::Crop { keyframes, .. }
            | Effect::Blur { keyframes, .. }
            | Effect::Color { keyframes, .. }
            | Effect::Volume { keyframes, .. } => keyframes,
            Effect::Blend { .. } | Effect::Fade { .. } | Effect::AudioFade { .. } | Effect::Mlt { .. } => &[],
        }
    }
}

/// A transition between two adjacent media clips on the same track. The cut point is the
/// boundary between them; the transition spans `duration` seconds centred on it, so both
/// clips are extended by `duration/2` **in source time** (scaled by their speed): the `to`
/// clip needs that much handle before its `in` point, the `from` clip after its out point.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub from: ClipId,
    pub to: ClipId,
    #[serde(default)]
    pub kind: TransitionKind,
    pub duration: f64,
    /// Wipe pattern: one of [`LUMAS`] or a PGM/PNG path relative to the project. Only for `wipe`.
    #[serde(default)]
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

/// Built-in wipe patterns (generated by the compiler; no luma files needed).
pub const LUMAS: &[&str] = &["wipe_left", "wipe_right", "wipe_up", "wipe_down", "iris", "diagonal"];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub t: f64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn example_project_round_trips() {
        let json = r#"{
          "version": 1, "name": "demo", "master": "main",
          "fps": {"num": 30000, "den": 1001}, "width": 1920, "height": 1080,
          "bin": {
            "screen": { "name": "Screen recording", "source": { "path": "media/screen.mp4", "kind": "video" } },
            "third":  { "name": "Intro title", "source": { "template": "lower-third", "vars": { "title": "kuma" } } },
            "main": { "name": "Main", "source": { "tracks": [
              { "id": "v1", "kind": "video", "clips": [
                { "id": "c1", "start": 0.0, "end": 12.5, "bin": "screen", "in": 3.2,
                  "effects": [
                    { "type": "crop", "left": 0.05, "right": 0.05 },
                    { "type": "transform", "keyframes": [
                      { "t": 0.0, "scale": 1.0 }, { "t": 3.0, "scale": 1.4, "x": -120 } ] } ] } ] },
              { "id": "o1", "kind": "overlay", "clips": [
                { "id": "o_c1", "start": 2.0, "end": 8.0, "bin": "third", "speed": 0.5,
                  "effects": [ { "type": "blend", "mode": "screen" }, { "type": "blur", "radius": 2 } ] } ] }
            ] } }
          }
        }"#;
        let p: Project = serde_json::from_str(json).expect("parse");
        let tracks = &p.master_seq().expect("master is a sequence").tracks;
        assert!(p.item(&tracks[1].clips[0]).unwrap().is_overlay());
        assert!(p.item(&tracks[0].clips[0]).unwrap().is_timed_media());
        assert_eq!(p.duration(), 12.5);
    }

    fn seq(tracks: Value) -> Value {
        json!({ "source": { "tracks": tracks } })
    }

    /// One keyframe type for every effect: the parameters are its flattened fields, so a
    /// transform's `{ t, x, scale }` and a volume's `{ t, gain }` are both instances of
    /// it — and come back out exactly as they went in, `ease` included only when set.
    #[test]
    fn a_keyframe_round_trips_as_the_effects_own_parameter_names() {
        let k: Keyframe = serde_json::from_value(json!({ "t": 1.5, "x": 120, "scale": 1.2 })).unwrap();
        assert_eq!(k.get("x"), Some(120.0));
        assert_eq!(k.or("y", 7.0), 7.0, "a parameter the key leaves out is the static value");
        assert_eq!(serde_json::to_value(&k).unwrap(), json!({ "t": 1.5, "scale": 1.2, "x": 120.0 }));
        let held: Keyframe = serde_json::from_value(json!({ "t": 0, "gain": 0.5, "ease": "hold" })).unwrap();
        assert_eq!(serde_json::to_value(&held).unwrap(), json!({ "t": 0.0, "ease": "hold", "gain": 0.5 }));
        // A parameter that is not a number is not a parameter.
        assert!(serde_json::from_value::<Keyframe>(json!({ "t": 0, "x": "far" })).is_err());
        // A kind that takes keyframes accepts them; the format is what the field says.
        let e: Effect = serde_json::from_value(json!({ "type": "color", "keyframes": [ { "t": 0, "brightness": -0.2 } ] })).unwrap();
        assert_eq!(e.keyframes()[0].or("brightness", 0.0), -0.2);
        assert_eq!(e.animatable(), ["brightness", "contrast", "saturation", "gamma"]);
        // …and none is written when there are none, so a file without them is unchanged.
        let e: Effect = serde_json::from_value(json!({ "type": "blur", "radius": 3 })).unwrap();
        assert_eq!(serde_json::to_value(&e).unwrap(), json!({ "type": "blur", "radius": 3.0 }));
    }

    /// A volume is static until it is given keys, like every other kind that moves, and a
    /// volume written before it had a static gain reads exactly as it did: a key without
    /// `gain` meant 1, and the static it now falls back to is 1.
    #[test]
    fn a_volume_is_static_until_keyed_and_an_old_one_reads_the_same() {
        let e: Effect = serde_json::from_value(json!({ "type": "volume", "gain": 0.5 })).unwrap();
        assert_eq!(serde_json::to_value(&e).unwrap(), json!({ "type": "volume", "gain": 0.5 }), "no keys, none written");
        let old: Effect = serde_json::from_value(json!({ "type": "volume", "keyframes": [ { "t": 0, "gain": 0 }, { "t": 1 } ] })).unwrap();
        let Effect::Volume { gain, keyframes } = &old else { panic!("{old:?}") };
        assert_eq!((*gain, keyframes[1].or("gain", *gain)), (1.0, 1.0));
        let fade: Effect = serde_json::from_value(json!({ "type": "audio_fade", "in": 1, "out": 2 })).unwrap();
        assert_eq!(fade.streams(), Streams::Audio);
        assert!(fade.animatable().is_empty(), "a fade's numbers are lengths of time");
    }

    /// Which effects can act on a clip: picture where the track draws one and the source
    /// has one, sound where the source has it — and a mute changes neither.
    #[test]
    fn a_clip_carries_what_its_source_has_where_its_track_allows() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "talk": { "source": { "path": "talk.mp4" } },
                "screen": { "source": { "path": "screen.mp4" }, "meta": { "has_audio": false } },
                "song": { "source": { "path": "song.mp3", "kind": "audio" } },
                "logo": { "source": { "path": "logo.png", "kind": "image" } },
                "red": { "source": { "color": "#f00" } },
                "tag": { "source": { "template": "location-tag" } },
                "part": seq(json!([{ "id": "pa", "kind": "audio", "clips": [ { "id": "p1", "start": 0, "end": 2, "bin": "song" } ] }])),
                "main": seq(json!([
                    { "id": "v1", "clips": [
                        { "id": "c1", "start": 0, "end": 1, "bin": "talk", "muted": true },
                        { "id": "c2", "start": 1, "end": 2, "bin": "screen" },
                        { "id": "c3", "start": 2, "end": 3, "bin": "logo" },
                        { "id": "c4", "start": 3, "end": 4, "bin": "red" },
                        { "id": "c5", "start": 4, "end": 5, "bin": "part" } ] },
                    { "id": "o1", "kind": "overlay", "clips": [ { "id": "c6", "start": 0, "end": 1, "bin": "tag" } ] },
                    { "id": "a1", "kind": "audio", "clips": [
                        { "id": "c7", "start": 0, "end": 1, "bin": "song" },
                        { "id": "c8", "start": 1, "end": 2, "bin": "talk" },
                        { "id": "c9", "start": 2, "end": 3, "bin": "screen" } ] }
                ]))
            }
        }))
        .unwrap();
        let of = |id: &str| {
            let (_, t, c) = p.find_clip(id).unwrap();
            p.clip_streams(t, c)
        };
        assert_eq!(of("c1"), Some(Streams::Both), "a mute is a state, not a capability");
        assert_eq!(of("c2"), Some(Streams::Video), "the probe found no sound");
        assert_eq!(of("c3"), Some(Streams::Video));
        assert_eq!(of("c4"), Some(Streams::Video));
        assert_eq!(of("c5"), Some(Streams::Both), "a sequence sounds when something in it does");
        assert_eq!(of("c6"), Some(Streams::Video));
        assert_eq!(of("c7"), Some(Streams::Audio));
        assert_eq!(of("c8"), Some(Streams::Audio), "an audio track draws no picture");
        assert_eq!(of("c9"), None, "a silent video on an audio track has nothing to act on");

        let fade: Effect = serde_json::from_value(json!({ "type": "fade", "in": 1 })).unwrap();
        let volume: Effect = serde_json::from_value(json!({ "type": "volume" })).unwrap();
        let mlt: Effect = serde_json::from_value(json!({ "type": "mlt", "service": "x" })).unwrap();
        assert!(!fade.acts_on(Streams::Audio) && volume.acts_on(Streams::Audio) && !volume.acts_on(Streams::Video));
        assert!(mlt.acts_on(Streams::Audio) && mlt.acts_on(Streams::Video), "the escape hatch is opaque, so it is offered everywhere");
    }

    /// A sequence is just a fourth kind of bin entry, so everything a clip already does
    /// applies to it: `in` offsets into its timeline, and its length is its own.
    #[test]
    fn a_sequence_is_a_bin_entry_a_clip_can_place() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "x": { "source": { "path": "x.mp4" } },
                "part": seq(json!([{ "id": "pv", "clips": [ { "id": "p1", "start": 0, "end": 6, "bin": "x" } ] }])),
                "main": seq(json!([{ "id": "v1", "clips": [ { "id": "c1", "start": 0, "end": 4, "bin": "part", "in": 2 } ] }]))
            }
        }))
        .unwrap();
        assert_eq!(p.seq_duration("part"), 6.0);
        assert_eq!(p.duration(), 4.0, "the master measures its own clips, not what they contain");
        let c1 = &p.master_seq().unwrap().tracks[0].clips[0];
        assert!(p.item(c1).unwrap().is_timed_media(), "a sequence has a timeline to offset into");
        // A clip anywhere is findable by id, and says which sequence it is on.
        assert_eq!(p.find_clip("p1").unwrap().0, "part");
        assert_eq!(p.compile_order("main").unwrap(), vec!["part", "main"], "inner sequences first");
    }

    #[test]
    fn a_sequence_containing_itself_is_reported_as_a_ring() {
        let p: Project = serde_json::from_value(json!({
            "bin": {
                "a": seq(json!([{ "id": "av", "clips": [ { "id": "ac", "start": 0, "end": 4, "bin": "b" } ] }])),
                "b": seq(json!([{ "id": "bv", "clips": [ { "id": "bc", "start": 0, "end": 4, "bin": "a" } ] }])),
                "main": seq(json!([{ "id": "v1", "clips": [ { "id": "c1", "start": 0, "end": 4, "bin": "a" } ] }]))
            }
        }))
        .unwrap();
        // The chain closes on the entry it started from, so the message can name the ring.
        assert_eq!(p.compile_order("main").unwrap_err(), vec!["a", "b", "a"]);
        assert!(p.compile_order("b").is_err());
        // Nothing hangs on a cyclic document: these are the calls the daemon still makes.
        assert!(!p.seq_has_audio("a"));
        assert_eq!(p.seq_duration("a"), 4.0);
    }

    #[test]
    fn audio_is_found_through_nesting() {
        let mk = |muted: bool| -> Project {
            serde_json::from_value(json!({
                "bin": {
                    "song": { "source": { "path": "song.mp3", "kind": "audio" } },
                    "still": { "source": { "path": "logo.png", "kind": "image" } },
                    "inner": seq(json!([{ "id": "ia", "kind": "audio", "muted": muted,
                        "clips": [ { "id": "i1", "start": 0, "end": 4, "bin": "song" } ] }])),
                    "main": seq(json!([
                        { "id": "v1", "clips": [ { "id": "c1", "start": 0, "end": 4, "bin": "still" } ] },
                        { "id": "v2", "clips": [ { "id": "c2", "start": 0, "end": 4, "bin": "inner" } ] }
                    ]))
                }
            }))
            .unwrap()
        };
        assert!(mk(false).seq_has_audio("main"), "sound two levels down still needs a mix");
        assert!(!mk(true).seq_has_audio("main"), "a muted track inside carries nothing out");
    }

    #[test]
    fn a_minted_bin_id_reads_like_a_typed_one() {
        let p: Project = serde_json::from_value(json!({ "bin": {
            "talk-subs": { "source": { "color": "#000" } },
            "talk-subs-2": { "source": { "color": "#000" } }
        } }))
        .unwrap();
        assert_eq!(p.free_bin_id("talk-subs"), "talk-subs-3");
        assert_eq!(p.free_bin_id("other"), "other");
    }

    #[test]
    fn children_are_ordered_then_by_id() {
        let p: Project = serde_json::from_value(json!({ "bin": {
            "a": { "source": { "color": "#000" } },
            "b": { "source": { "color": "#000" }, "order": 1 },
            "c": { "source": { "color": "#000" }, "order": 0 },
            "d": { "source": { "color": "#000" }, "parent": "a" }
        } }))
        .unwrap();
        assert_eq!(p.children(None).into_iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["c", "b", "a"]);
        assert_eq!(p.children(Some("a")).len(), 1);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = r#"{ "bin": { "m": { "source": { "tracks": [ { "id": "v1", "clipz": [] } ] } } } }"#;
        assert!(serde_json::from_str::<Project>(json).is_err());
        // Also inside the untagged source enum, where serde would otherwise drop them.
        let json = r#"{ "bin": { "a": { "source": { "path": "x.mp4", "offset": 3 } } } }"#;
        assert!(serde_json::from_str::<Project>(json).is_err());
        // `tracks` is what tells a sequence apart from the other sources, so it is
        // required: an entry with no recognisable source must not become an empty timeline.
        let json = r#"{ "bin": { "a": { "source": { } } } }"#;
        assert!(serde_json::from_str::<Project>(json).is_err());
    }

    #[test]
    fn ids_are_xml_and_url_safe() {
        assert!(is_valid_id("c1") && is_valid_id("_x-2") && is_valid_id("o_paris"));
        assert!(!is_valid_id("") && !is_valid_id("1c") && !is_valid_id("a b") && !is_valid_id("a/b"));
    }
}
