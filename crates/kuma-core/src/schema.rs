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

/// Stable content hash of a JSON value (used for bake caching).
pub fn hash_json(v: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(serde_json::to_vec(v).unwrap_or_default());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema_generates() {
        let s = super::project_schema();
        let v = serde_json::to_value(&s).unwrap();
        assert!(v["properties"]["tracks"].is_object());
    }
}
