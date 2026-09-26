//! The agent skill that ships with kuma (`library/skills/kuma-video/`): how to make a whole
//! video through the MCP, and the traps found doing it. `kuma init` and `kuma mcp-config`
//! write it into the project's `.claude/skills/`, where Claude Code picks up a project's
//! skills. It is embedded, so a project gets the version that documents this binary.

use std::path::{Path, PathBuf};

pub const NAME: &str = "kuma-video";

macro_rules! skill_files {
    ($($rel:literal),* $(,)?) => {
        &[$(($rel, include_str!(concat!("../../../library/skills/kuma-video/", $rel)))),*]
    };
}

/// Every file of the skill, by its path inside the skill folder.
pub const FILES: &[(&str, &str)] = skill_files![
    "SKILL.md",
    "references/storyboard-as-code.md",
    "references/overlays.md",
    "references/japanese-type.md",
    "references/audio.md",
    "references/pacing.md",
    "references/footage.md",
    "references/render-and-review.md",
    "scripts/mixsim.py",
    "scripts/fontcheck.py",
    "scripts/subset_jp_fonts.py",
    "scripts/reading_audit.py",
    "scripts/contact_sheet.py",
    "scripts/pixel_footage_template.py",
];

/// Where the skill lives in a project.
pub fn dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("skills").join(NAME)
}

/// Write the skill into `<project_dir>/.claude/skills/kuma-video/`. kuma's own files are
/// replaced — they document this kuma, and a stale copy misleads — and anything else in
/// the folder is left alone.
pub fn install(project_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = dir(project_dir);
    for (rel, text) in FILES {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, text)?;
        #[cfg(unix)]
        if rel.ends_with(".py") {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(dir)
}

/// Whether the project has the skill, and whether it is this kuma's.
pub fn status(project_dir: &Path) -> &'static str {
    let dir = dir(project_dir);
    if !dir.join("SKILL.md").is_file() {
        return "missing";
    }
    let current = FILES.iter().all(|(rel, want)| std::fs::read_to_string(dir.join(rel)).is_ok_and(|have| have == *want));
    if current { "current" } else { "stale" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library/skills").join(NAME)
    }

    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else if !p.extension().is_some_and(|x| x == "pyc") {
                out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }

    /// A file added to the skill but not to `FILES` would never reach a project.
    #[test]
    fn every_file_of_the_skill_is_embedded() {
        let mut on_disk = Vec::new();
        walk(&source_dir(), &source_dir(), &mut on_disk);
        on_disk.sort();
        let mut embedded: Vec<String> = FILES.iter().map(|(rel, _)| rel.to_string()).collect();
        embedded.sort();
        assert_eq!(on_disk, embedded);
    }

    /// Claude Code reads a skill's name and description from its front matter.
    #[test]
    fn the_skill_says_its_name_and_when_to_use_it() {
        let (rel, text) = FILES[0];
        assert_eq!(rel, "SKILL.md");
        let front = text.strip_prefix("---\n").and_then(|t| t.split_once("\n---\n")).map(|(f, _)| f).expect("front matter");
        assert!(front.lines().any(|l| l == format!("name: {NAME}")), "{front}");
        assert!(front.lines().any(|l| l.starts_with("description: ") && l.len() > 80), "{front}");
    }

    #[test]
    fn install_writes_every_file_and_leaves_the_rest() {
        let project = std::env::temp_dir().join(format!("kuma-skill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        let dir = dir(&project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "mine").unwrap();
        std::fs::write(dir.join("SKILL.md"), "an old copy").unwrap();
        assert_eq!(status(&project), "stale");
        install(&project).unwrap();
        assert_eq!(status(&project), "current");
        for (rel, text) in FILES {
            assert_eq!(std::fs::read_to_string(dir.join(rel)).unwrap(), *text, "{rel}");
        }
        assert_eq!(std::fs::read_to_string(dir.join("notes.md")).unwrap(), "mine", "someone's file is left alone");
        std::fs::remove_dir_all(&project).unwrap();
        assert_eq!(status(&project), "missing");
    }
}
