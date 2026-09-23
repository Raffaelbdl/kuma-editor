//! JSON Schema export — the machine-checkable contract handed to agents.

use schemars::{schema_for, Schema};

use crate::model::Project;
use crate::manifest::OverlayManifest;

pub fn project_schema() -> Schema {
    schema_for!(Project)
}

pub fn overlay_schema() -> Schema {
    schema_for!(OverlayManifest)
}
