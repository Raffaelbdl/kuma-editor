//! Files the compiler generates into the cache — wipe lumas, grades' LUTs — named by what
//! they are, so one written is never written again.

use std::path::{Path, PathBuf};

/// `dir/name`, written from `bytes()` if it is not there yet. A preview compile and a
/// render may write the same file at once, so it is written under a `.tmp-` name (what a
/// prune of the cache leaves alone) and renamed into place.
pub(crate) fn write_once(dir: &Path, name: &str, bytes: impl FnOnce() -> std::io::Result<Vec<u8>>) -> std::io::Result<PathBuf> {
    let out = dir.join(name);
    if !out.is_file() {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
        std::fs::write(&tmp, bytes()?)?;
        std::fs::rename(&tmp, &out)?;
    }
    Ok(out)
}
