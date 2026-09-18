//! kuma-overlay — turns an overlay template instance into an RGBA frame sequence.
//!
//! The bake is deterministic and cached: the cache key covers the template's files, the
//! resolved vars, fps, size, duration, speed, quality and the renderer version. Every bake
//! runs hyperframes with a single worker, because multi-worker frame assignment was shown
//! to change subpixel rasterisation of GSAP-driven transforms between runs.
//!
//! Output is an RGBA PNG sequence (lossless, straight alpha, read natively by MLT).

pub mod bake;
pub mod inject;
pub mod lint;
pub mod media;
pub mod scaffold;
pub mod transcribe;

pub use bake::{BakeError, BakeRequest, Baker, BakeResult};
pub use lint::{Finding, Level, LintReport};
pub use inject::Contract;
pub use media::{probe, Probe};
pub use scaffold::new_template;
pub use transcribe::{Cue, TranscribeOptions, Transcriber};
