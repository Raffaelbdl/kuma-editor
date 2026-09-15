//! JSON Schema export — the machine-checkable contract handed to agents.

use schemars::{schema_for, Schema};

use crate::model::Project;
use crate::template::TemplateManifest;

pub fn project_schema() -> Schema {
    schema_for!(Project)
}

pub fn template_schema() -> Schema {
    schema_for!(TemplateManifest)
}
