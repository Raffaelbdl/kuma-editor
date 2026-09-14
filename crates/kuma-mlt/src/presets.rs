//! Export presets: `melt -consumer avformat:<out> k=v ...`.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ExportPreset {
    pub id: &'static str,
    pub description: &'static str,
    pub extension: &'static str,
    /// Consumer properties passed to `melt`.
    pub properties: &'static [(&'static str, &'static str)],
}

/// Clean high-bitrate H.264 upload: YouTube re-encodes everything, so quality beats size.
/// Later keys override earlier ones on the melt command line.
macro_rules! youtube_h264 {
    ($crf:literal) => {
        &[
            ("f", "mp4"),
            ("vcodec", "libx264"),
            ("preset", "slow"),
            ("crf", $crf),
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

pub const PRESETS: &[ExportPreset] = &[
    ExportPreset {
        id: "youtube-1080p",
        description: "H.264 High, yuv420p, CRF 18, AAC 384k, faststart — clean YouTube upload",
        extension: "mp4",
        properties: youtube_h264!("18"),
    },
    ExportPreset {
        id: "youtube-4k",
        description: "H.264 High, yuv420p, CRF 17, AAC 384k — 4K upload",
        extension: "mp4",
        properties: youtube_h264!("17"),
    },
    ExportPreset {
        id: "youtube-shorts",
        description: "Same encode as youtube-1080p; use with a 1080×1920 project",
        extension: "mp4",
        properties: youtube_h264!("18"),
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
            ("pix_fmt", "yuv420p"),
            ("acodec", "libopus"),
            ("ab", "160k"),
        ],
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
    },
];

pub fn find(id: &str) -> Option<&'static ExportPreset> {
    PRESETS.iter().find(|p| p.id == id)
}
