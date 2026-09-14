//! kuma-overlay — turns an overlay template instance into an RGBA frame sequence.
//!
//! The bake is deterministic and cached: the cache key covers the template's files, the
//! resolved vars, fps, size, duration, speed, quality and the renderer version. Every bake
//! runs hyperframes with a single worker, because multi-worker frame assignment was shown
//! to change subpixel rasterisation of GSAP-driven transforms between runs.
//!
//! Output is a PNG sequence by default (lossless, straight alpha, read natively by MLT);
//! ProRes 4444 MOV is available for hand-off into other tools.

pub mod bake;
pub mod inject;
pub mod media;

pub use bake::{BakeError, BakeRequest, Baker, BakeResult, Format};
pub use inject::Contract;
pub use media::{probe, Probe};
