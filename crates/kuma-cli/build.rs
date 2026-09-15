// Stamp the git commit into `kuma --version` so a stale binary on PATH is obvious.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let hash = git(&["rev-parse", "--short=10", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=KUMA_GIT={hash}");
    // Re-run when the checked-out commit changes. `--git-path` resolves worktrees, where
    // `.git` is a file; a missing path would otherwise force a rebuild every time.
    for p in ["HEAD", "refs/heads", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", p]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
