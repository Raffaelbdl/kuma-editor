//! Export presets: `melt -consumer avformat:<out> k=v ...`.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ExportPreset {
    pub id: &'static str,
    pub description: &'static str,
    pub extension: &'static str,
    /// Consumer properties passed to `melt`.
    pub properties: &'static [(&'static str, &'static str)],
    /// The rate factor a render may set for itself, when the codec has one.
    pub quality: Option<Quality>,
    /// The encoder's speed a render may set for itself, when the codec has one.
    pub speed: Option<Speed>,
    /// The hardware it needs, when its encoder is not software (see `available`).
    pub needs: Option<&'static str>,
}

/// What one render changes of its preset. Each left out is the preset's own, or the
/// machine's for `threads`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tuning {
    /// Frames melt builds at once, 1 to [`MAX_THREADS`]; `melt::render_threads()` when absent.
    #[serde(default)]
    pub threads: Option<usize>,
    /// The rate factor, within the preset's `quality`.
    #[serde(default)]
    pub quality: Option<u32>,
    /// The encoder speed, one of the preset's `speed.options`.
    #[serde(default)]
    pub speed: Option<String>,
}

/// Most frames a render may build at once.
pub const MAX_THREADS: usize = 64;

impl ExportPreset {
    /// Refuses what the preset does not offer, rather than passing melt a key its codec
    /// would ignore.
    pub fn check(&self, t: &Tuning) -> Result<(), String> {
        if let Some(n) = t.threads.filter(|n| !(1..=MAX_THREADS).contains(n)) {
            return Err(format!("threads must be between 1 and {MAX_THREADS}, not {n}"));
        }
        if let Some(q) = t.quality {
            let range = self.quality.ok_or_else(|| format!("preset `{}` has no quality to set", self.id))?;
            if !(range.min..=range.max).contains(&q) {
                return Err(format!("quality {q} is outside {}–{} for `{}`", range.min, range.max, self.id));
            }
        }
        if let Some(v) = &t.speed {
            let speed = self.speed.ok_or_else(|| format!("preset `{}` has no encoder speed to set", self.id))?;
            if !speed.options.contains(&v.as_str()) {
                return Err(format!("speed `{v}` is not one of {} for `{}`", speed.options.join(", "), self.id));
            }
        }
        Ok(())
    }

    /// Its consumer properties, with `t`'s quality and speed in place of its own.
    pub fn tuned<'a>(&'a self, t: &'a Tuning) -> impl Iterator<Item = (&'static str, String)> + 'a {
        self.properties.iter().map(move |&(k, v)| {
            let quality = self.quality.filter(|q| q.key == k).and(t.quality).map(|q| q.to_string());
            let speed = self.speed.filter(|s| s.key == k).and(t.speed.clone());
            (k, quality.or(speed).unwrap_or_else(|| v.to_string()))
        })
    }
}

/// A preset as this machine offers it: `unavailable` says why it cannot run here.
#[derive(Debug, Serialize)]
pub struct Offered {
    #[serde(flatten)]
    pub preset: &'static ExportPreset,
    pub unavailable: Option<String>,
}

/// Every preset, each with whether this machine can run it. Can take a second the first
/// time (`available`).
pub fn offered() -> Vec<Offered> {
    PRESETS.iter().map(|preset| Offered { preset, unavailable: available(preset).err() }).collect()
}

/// A constant rate factor: lower is better and larger. `default` is the preset's own, and
/// is one of its `properties`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Quality {
    /// The consumer property it sets.
    pub key: &'static str,
    pub default: u32,
    pub min: u32,
    pub max: u32,
}

/// An encoder's speed against compression. `default` is the preset's own, and is one of
/// its `properties`; the picture's quality is the rate factor's, so a faster speed costs
/// file size at the same `Quality`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Speed {
    /// The consumer property it sets.
    pub key: &'static str,
    /// What the encoder calls it, for a tooltip.
    pub label: &'static str,
    pub default: &'static str,
    /// Fastest first.
    pub options: &'static [&'static str],
}

const X264_CRF: Quality = Quality { key: "crf", default: 23, min: 0, max: 51 };
const X264_SPEED: Speed = Speed {
    key: "preset",
    label: "x264 preset",
    default: "medium",
    options: &["ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower", "veryslow"],
};

/// Clean high-bitrate H.264 upload: YouTube re-encodes everything, so quality beats size.
macro_rules! youtube_h264 {
    ($crf:literal) => {
        &[
            ("f", "mp4"),
            ("vcodec", "libx264"),
            ("preset", "slow"),
            ("crf", stringify!($crf)),
            ("profile", "high"),
            ("pix_fmt", "yuv420p"),
            ("g", "60"),
            ("acodec", "aac"),
            ("ab", "384k"),
            ("ar", "48000"),
            ("movflags", "+faststart"),
        ]
    };
}

/// The graph's sound alone, as 32-bit float WAV: what `measure_audio` meters. Not offered
/// for export. Float, because kuma has no limiter and a mix can go over full scale, which
/// 16-bit samples would clip before the meter saw it.
pub static AUDIO_ONLY: ExportPreset = ExportPreset {
    id: "audio-only",
    description: "the sound of a sequence as float WAV, for measuring",
    extension: "wav",
    properties: &[("f", "wav"), ("acodec", "pcm_f32le"), ("ac", "2"), ("vn", "1")],
    quality: None,
    speed: None,
    needs: None,
};

pub const PRESETS: &[ExportPreset] = &[
    ExportPreset {
        id: "youtube-1080p",
        description: "H.264 High, yuv420p, CRF 18, AAC 384k, faststart — clean YouTube upload",
        extension: "mp4",
        properties: youtube_h264!(18),
        quality: Some(Quality { default: 18, ..X264_CRF }),
        speed: Some(Speed { default: "slow", ..X264_SPEED }),
        needs: None,
    },
    ExportPreset {
        id: "youtube-4k",
        description: "H.264 High, yuv420p, CRF 17, AAC 384k — 4K upload",
        extension: "mp4",
        properties: youtube_h264!(17),
        quality: Some(Quality { default: 17, ..X264_CRF }),
        speed: Some(Speed { default: "slow", ..X264_SPEED }),
        needs: None,
    },
    ExportPreset {
        id: "youtube-nvenc",
        description: "H.264 High on the NVIDIA GPU (NVENC p6, CQ 21), AAC 384k, faststart — a YouTube upload, faster than x264 and a larger file",
        extension: "mp4",
        // CQ 21 and p6 were calibrated against `youtube-1080p` (*Bake and render speed*).
        properties: &[
            ("f", "mp4"),
            ("vcodec", "h264_nvenc"),
            ("preset", "p6"),
            ("tune", "hq"),
            ("rc", "vbr"),
            ("cq", "21"),
            ("vb", "0"),
            ("profile", "high"),
            ("pix_fmt", "yuv420p"),
            ("g", "60"),
            ("bf", "3"),
            ("acodec", "aac"),
            ("ab", "384k"),
            ("ar", "48000"),
            ("movflags", "+faststart"),
        ],
        // 0 is not a quality to NVENC but "choose for me".
        quality: Some(Quality { key: "cq", default: 21, min: 1, max: 51 }),
        speed: Some(Speed { key: "preset", label: "NVENC preset, p1 fastest", default: "p6", options: &["p1", "p2", "p3", "p4", "p5", "p6", "p7"] }),
        needs: Some("an NVIDIA GPU with NVENC"),
    },
    ExportPreset {
        id: "draft",
        description: "Fast H.264 draft (ultrafast, CRF 26) for quick checks",
        extension: "mp4",
        properties: &[
            ("f", "mp4"),
            ("vcodec", "libx264"),
            ("preset", "ultrafast"),
            ("crf", "26"),
            ("pix_fmt", "yuv420p"),
            ("acodec", "aac"),
            ("ab", "128k"),
            ("movflags", "+faststart"),
        ],
        quality: Some(Quality { default: 26, ..X264_CRF }),
        speed: Some(Speed { default: "ultrafast", ..X264_SPEED }),
        needs: None,
    },
    ExportPreset {
        id: "vp9-hq",
        description: "WebM VP9 CRF 24, Opus 160k",
        extension: "webm",
        properties: &[
            ("f", "webm"),
            ("vcodec", "libvpx-vp9"),
            ("crf", "24"),
            ("vb", "0"),
            // libvpx's own default, written out so the dialog's default is what is passed.
            ("cpu-used", "1"),
            ("pix_fmt", "yuv420p"),
            ("acodec", "libopus"),
            ("ab", "160k"),
        ],
        quality: Some(Quality { key: "crf", default: 24, min: 0, max: 63 }),
        // `cpu-used` beyond 5 is for the realtime deadline; at `good`, 0 to 5.
        speed: Some(Speed { key: "cpu-used", label: "libvpx cpu-used (higher is faster)", default: "1", options: &["5", "4", "3", "2", "1", "0"] }),
        needs: None,
    },
    ExportPreset {
        id: "prores-master",
        description: "ProRes 422 HQ master, PCM audio",
        extension: "mov",
        properties: &[
            ("f", "mov"),
            ("vcodec", "prores_ks"),
            ("vprofile", "3"),
            ("pix_fmt", "yuv422p10le"),
            ("acodec", "pcm_s24le"),
        ],
        // A ProRes profile is its quality, and prores_ks has no speed to trade.
        quality: None,
        speed: None,
        needs: None,
    },
];

/// Whether this machine can render with `preset`, and why not. Software encoders always
/// can. One that `needs` hardware is tried once per process (`melt::try_preset`): ffmpeg
/// lists NVENC whether or not there is a GPU, and melt crashes rendering with it where
/// there is none.
pub fn available(preset: &ExportPreset) -> Result<(), String> {
    if preset.needs.is_none() {
        return Ok(());
    }
    // Held while trying, so two first asks try once.
    static TRIED: Mutex<BTreeMap<&str, Result<(), String>>> = Mutex::new(BTreeMap::new());
    let mut tried = TRIED.lock().unwrap_or_else(PoisonError::into_inner);
    tried.entry(preset.id).or_insert_with(|| crate::melt::try_preset(preset)).clone()
}

pub fn find(id: &str) -> Option<&'static ExportPreset> {
    PRESETS.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the dialog shows as a preset's own quality and speed is what the preset passes.
    #[test]
    fn a_tunable_default_is_the_presets_own_value() {
        for p in PRESETS {
            let has = |k: &str, v: &str| p.properties.iter().any(|&(pk, pv)| pk == k && pv == v);
            if let Some(q) = p.quality {
                assert!(has(q.key, &q.default.to_string()), "{}: {}={} is not in its properties", p.id, q.key, q.default);
                assert!(q.min <= q.default && q.default <= q.max, "{}", p.id);
            }
            if let Some(s) = p.speed {
                assert!(has(s.key, s.default), "{}: {}={} is not in its properties", p.id, s.key, s.default);
                assert!(s.options.contains(&s.default), "{}", p.id);
            }
        }
    }

    /// A render may set its frames at once, and the preset's quality and speed where it
    /// has them, which then replace the preset's own; anything else is refused.
    #[test]
    fn a_tuning_is_checked_and_replaces_the_presets_own() {
        let youtube = find("youtube-1080p").unwrap();
        let t = Tuning { threads: Some(3), quality: Some(20), speed: Some("medium".into()) };
        assert_eq!(youtube.check(&t), Ok(()));
        let props: Vec<_> = youtube.tuned(&t).collect();
        assert!(props.contains(&("crf", "20".into())) && props.contains(&("preset", "medium".into())), "{props:?}");
        assert_eq!(props.len(), youtube.properties.len(), "replaced, not added");
        assert_eq!(youtube.tuned(&Tuning::default()).map(|(_, v)| v).collect::<Vec<_>>(), youtube.properties.iter().map(|(_, v)| v.to_string()).collect::<Vec<_>>());

        let refused = |id, t: Tuning| find(id).unwrap().check(&t).unwrap_err();
        assert_eq!(refused("youtube-1080p", Tuning { threads: Some(0), ..Tuning::default() }), "threads must be between 1 and 64, not 0");
        assert_eq!(refused("youtube-1080p", Tuning { quality: Some(52), ..Tuning::default() }), "quality 52 is outside 0–51 for `youtube-1080p`");
        assert_eq!(
            refused("youtube-1080p", Tuning { speed: Some("turbo".into()), ..Tuning::default() }),
            "speed `turbo` is not one of ultrafast, superfast, veryfast, faster, fast, medium, slow, slower, veryslow for `youtube-1080p`"
        );
        assert_eq!(refused("prores-master", Tuning { quality: Some(10), ..Tuning::default() }), "preset `prores-master` has no quality to set");
        assert_eq!(refused("prores-master", Tuning { speed: Some("fast".into()), ..Tuning::default() }), "preset `prores-master` has no encoder speed to set");
        assert_eq!(find("prores-master").unwrap().check(&Tuning { threads: Some(2), ..Tuning::default() }), Ok(()), "any preset takes threads");
    }
}
