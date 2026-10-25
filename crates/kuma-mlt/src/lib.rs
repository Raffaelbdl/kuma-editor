//! kuma-mlt — the only crate that knows MLT's sharp edges.
//!
//! * seconds → frames happens exactly once, via [`kuma_core::Fps`];
//! * `in`/`out` are inclusive frame indices, `<blank length>` is a duration;
//! * every overlaid track gets an explicit `qtblend` transition and a `mix`;
//! * a black background track is always track 0;
//! * dissolves/wipes are compiled to an A/B-lane tractor per track with `luma`.
//!
//! The emitted document is renderable by `melt` and openable by Kdenlive.

pub mod compile;
pub mod lumas;
pub mod luts;
pub mod melt;
pub mod presets;
pub mod xml;

pub use compile::{compile, CompileError, CompileOptions, OverlayBake, UnbakedPolicy};
pub use kuma_core::Streams;
pub use melt::{render, MeltError, RenderJob, RenderProgress};
pub use presets::{ExportPreset, Tuning, PRESETS};
