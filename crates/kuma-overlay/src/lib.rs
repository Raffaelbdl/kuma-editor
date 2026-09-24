//! kuma-overlay — turns an overlay (a folder of HTML plus vars) into an RGBA frame sequence.
//!
//! The bake is deterministic and cached: the cache key covers the overlay's files, the
//! resolved vars, fps, size, duration, speed, quality and the renderer version. Every bake
//! runs hyperframes with a single worker, because multi-worker frame assignment was shown
//! to change subpixel rasterisation of GSAP-driven transforms between runs.
//!
//! Output is an RGBA PNG sequence (lossless, straight alpha, read natively by MLT).

pub mod bake;
pub mod fonts;
pub mod inject;
pub mod lint;
pub mod media;
pub mod scaffold;
pub mod transcribe;

pub use bake::{copy_dir, BakeError, BakeRequest, Baker, BakeResult};
pub use fonts::{Family, FontLibrary, Origin};
pub use lint::{Finding, Level, LintReport};
pub use inject::{Contract, FontCss};
pub use media::{probe, Probe};
pub use scaffold::new_overlay;
pub use transcribe::{Cue, TranscribeOptions, Transcriber};
