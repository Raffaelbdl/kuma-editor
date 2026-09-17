//! A new template that already obeys the contract.
//!
//! Authoring one from the prose costs an agent minutes and still gets the two easy things
//! wrong — the timeline registration and the root's timing attributes — which a bake only
//! reports 45 seconds later. Starting from a skeleton that bakes, and editing it, is the
//! cheaper loop and the one the contract was written for: the shape is here, the design is
//! the author's.
//!
//! What it writes is deliberately small and deliberately complete: one timed child, two
//! vars of different types, an entrance, an exit anchored to the clip's real length, and
//! sizing in `--u` so it survives both orientations. It lints clean, which a test pins.

use std::path::{Path, PathBuf};

/// Write `overlays/<id>/` under `dir` (a library's `overlays/` or a project's own).
/// Refuses to overwrite: a template being edited is somebody's work.
pub fn new_template(dir: &Path, id: &str, name: &str, description: &str) -> Result<PathBuf, String> {
    if !kuma_core::is_valid_id(id) {
        return Err(format!("`{id}` must match [A-Za-z_][A-Za-z0-9_-]*: the id is the folder name and a URL segment"));
    }
    let out = dir.join(id);
    if out.exists() {
        return Err(format!("{} already exists", out.display()));
    }
    let name = if name.is_empty() { id } else { name };
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let write = |file: &str, body: String| -> Result<(), String> {
        std::fs::write(out.join(file), body).map_err(|e| format!("{file}: {e}"))
    };
    write("template.json", manifest(id, name, description))?;
    write("index.html", INDEX.replace("{{NAME}}", &escape(name)))?;
    Ok(out)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn manifest(id: &str, name: &str, description: &str) -> String {
    let m = serde_json::json!({
        "id": id,
        "name": name,
        "description": if description.is_empty() { "A new overlay." } else { description },
        "duration_default": 4.0,
        "static": false,
        "vars": {
            "label": { "type": "string", "default": name, "label": "Text" },
            "accent": { "type": "color", "default": "#ff4f00" },
            "at": { "type": "point", "default": { "x": 0.5, "y": 0.8 }, "label": "Position" },
            "size": { "type": "number", "default": 1.0, "min": 0.5, "max": 2.5, "step": 0.1 }
        },
        "tags": []
    });
    format!("{}\n", serde_json::to_string_pretty(&m).unwrap())
}

/// The skeleton. Every line of it is contract: the root's `data-*` (rewritten to the
/// clip's real length before the renderer sees the file), `--u` and fractional positions
/// so it works in both orientations, the standalone defaults repeated in `kumaTemplate`,
/// and an exit anchored to `duration` rather than to a number.
const INDEX: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <script src="_vendor/gsap.min.js"></script>
    <script src="_vendor/kuma.js"></script>
    <link rel="stylesheet" href="_vendor/kuma.css" />
    <style>
      /* Size in `--u` (1vh × the `size` var) and place by fraction, so the same markup
         works at 1920×1080 and 1080×1920. Pixels are the one thing that does not travel. */
      .card {
        position: absolute;
        left: calc(var(--var-at-x) * 100%);
        top: calc(var(--var-at-y) * 100%);
        transform: translate(-50%, -50%);
        padding: calc(1.2 * var(--u)) calc(2.2 * var(--u));
        border-radius: calc(0.8 * var(--u));
        background: rgba(12, 12, 18, 0.82);
        border-left: calc(0.5 * var(--u)) solid var(--var-accent);
        color: #fff;
        font-family: "Segoe UI", system-ui, sans-serif;
        font-size: calc(3 * var(--u));
        font-weight: 600;
        white-space: nowrap;
        will-change: transform, opacity;
      }
    </style>
  </head>
  <body>
    <!-- One root. `data-duration` here is only the standalone default: kuma rewrites it
         to the clip's real length, and a timed child whose end matches the root's is
         stretched with it. -->
    <div id="root" data-composition-id="main" data-start="0" data-duration="4" data-width="1920" data-height="1080">
      <div id="card" class="clip card" data-start="0" data-duration="4">{{NAME}}</div>
    </div>
    <script>
      // The defaults are repeated from template.json so the page also works opened on its
      // own; kuma overrides them with the entry's vars.
      const { vars, duration, text, timeline } = kumaTemplate({
        label: "{{NAME}}", accent: "#ff4f00", at: { x: 0.5, y: 0.8 }, size: 1,
      });
      text("card", vars.label);

      // One paused timeline, registered for the renderer to seek. Never animate from the
      // wall clock: `setTimeout`, `Date.now()` and friends render wrong or frozen.
      const tl = timeline();
      tl.from("#card", { y: 24, opacity: 0, duration: 0.45, ease: "power3.out" }, 0);
      // The exit is anchored to the clip's real length, never to a number.
      tl.to("#card", { opacity: 0, duration: 0.35, ease: "power2.in" }, Math.max(0.5, duration - 0.35));
    </script>
  </body>
</html>
"##;
