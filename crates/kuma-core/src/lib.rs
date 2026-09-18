//! kuma-core — the project model that both the UI and the agent edit.
//!
//! Design rules (see the plan in the repo docs):
//! * Times are **seconds** (`f64`); clip `end` is **exclusive**.
//! * Frame rate is **rational** (`Fps { num, den }`), never an integer.
//! * This crate knows nothing about MLT. Every MLT sharp edge lives in `kuma-mlt`.
//! * The JSON shape is the public contract: it is exported as a JSON Schema so an
//!   agent can validate its own edits before sending them.

pub mod edit;
pub mod model;
pub mod schema;
pub mod template;
pub mod time;
pub mod upgrade;
pub mod validate;

pub use edit::{apply_patch, EditError};
pub use model::*;
pub use upgrade::parse_project;
pub use template::{TemplateManifest, VarSpec, VarType};
pub use time::{ClipFrames, Fps};
pub use validate::{validate, Issue, Severity, ValidationContext};

/// Current on-disk project format version.
///
/// 3 — crop, blur and color take `keyframes`, through the one `Keyframe` type every
///     keyframed effect now uses.
/// 2 — keyframes gained `ease` (`linear`, `hold`, `smooth`).
/// 1 — the first format.
pub const PROJECT_VERSION: u32 = 3;
