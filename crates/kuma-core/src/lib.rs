//! kuma-core — the project model that both the UI and the agent edit.
//!
//! Design rules (see the plan in the repo docs):
//! * Times are **seconds** (`f64`); clip `end` is **exclusive**.
//! * Frame rate is **rational** (`Fps { num, den }`), never an integer.
//! * This crate knows nothing about MLT. Every MLT sharp edge lives in `kuma-mlt`.
//! * The JSON shape is the public contract: it is exported as a JSON Schema so an
//!   agent can validate its own edits before sending them.

pub mod edit;
pub mod manifest;
pub mod model;
pub mod schema;
pub mod time;
pub mod upgrade;
pub mod validate;

pub use edit::{apply_patch, diff, EditError};
pub use model::*;
pub use manifest::{OverlayManifest, VarSpec, VarType};
pub use upgrade::{canonical, parse_project, project_text};
pub use time::{ClipFrames, Fps};
pub use validate::{validate, Issue, Severity, ValidationContext, LIBRARY_KINDS};

/// Current on-disk project format version.
///
/// 10 — a project may name the asset `libraries` it uses besides the user's own.
/// 9 — a marker may carry an `origin`: who left it (the daemon stamps it).
/// 8 — a sequence carries `effects` on its mix (`volume`, `limiter`, `auto_level`); the
///     effect kinds `limiter`, `auto_level` and `freeze` are new.
/// 7 — an overlay entry names its own folder in the project (`overlay`), no longer a
///     library template id (`template`). Not upgraded: see `upgrade.rs`.
/// 6 — a bin entry may carry a `volume`.
/// 5 — `volume` has a static `gain` and optional keyframes, like every kind that moves;
///     `audio_fade` is new.
/// 4 — a clip may carry a `group`: an editing relation, invisible to the compiler.
/// 3 — crop, blur and color take `keyframes`, through the one `Keyframe` type every
///     keyframed effect now uses.
/// 2 — keyframes gained `ease` (`linear`, `hold`, `smooth`).
/// 1 — the first format.
pub const PROJECT_VERSION: u32 = 10;
