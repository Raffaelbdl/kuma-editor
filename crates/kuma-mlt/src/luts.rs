//! Grades, as the LUT files MLT applies. A [`Grade`] is computed by `kuma_core::grade` and
//! written as one `.cube` under the cache, named by a hash of everything it is made of —
//! its settings and the bytes of the LUTs it reads — so the same grade compiles to the
//! same path, a changed one to a new path (which is what tells the preview to reload),
//! and a file is written once however many clips share it.

use std::path::{Path, PathBuf};

use kuma_core::grade::{Grading, Lut};
use kuma_core::Grade;
use sha2::{Digest, Sha256};

/// Where the grades of a compile with this `cache_dir` are written.
pub fn dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("luts")
}

/// The file `grade` compiles to under `cache_dir`, written if it is not there yet; `None`
/// for a grade that changes nothing. Its LUT paths resolve against `root`.
pub fn resolve(grade: &Grade, root: &Path, cache_dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let Some((name, grading)) = plan(grade, root)? else { return Ok(None) };
    let dir = dir(cache_dir);
    let out = dir.join(&name);
    if !out.is_file() {
        // A preview compile and a render may write the same grade at once; `.tmp-` is
        // what a prune of the cache leaves alone.
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
        std::fs::write(&tmp, grading.cube())?;
        std::fs::rename(&tmp, &out)?;
    }
    Ok(Some(out))
}

/// What `grade` compiles to, without writing it: the file's name in [`dir`] and the grade
/// ready to sample, or `None` for a grade that changes nothing. A prune asks this to know
/// which files the project still uses.
pub fn plan<'g>(grade: &'g Grade, root: &Path) -> std::io::Result<Option<(String, Grading<'g>)>> {
    let mut hash = Sha256::new();
    hash.update(b"kuma grade\n");
    hash.update(serde_json::to_vec(grade).map_err(std::io::Error::other)?);
    let mut read = |path: &Option<String>| -> std::io::Result<Option<Lut>> {
        let Some(path) = path else { return Ok(None) };
        let bytes = std::fs::read(root.join(path)).map_err(|e| std::io::Error::new(e.kind(), format!("LUT `{path}`: {e}")))?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        let lut = Lut::parse(&String::from_utf8_lossy(&bytes)).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("LUT `{path}`: {e}")))?;
        Ok(Some(lut))
    };
    let (input, look) = (read(&grade.input_lut)?, read(&grade.look)?);
    let grading = Grading::new(grade, input, look);
    if grading.is_neutral() {
        return Ok(None);
    }
    let hex: String = hash.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect();
    Ok(Some((format!("{hex}.cube"), grading)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kuma_core::Rgb;

    #[test]
    fn a_grade_is_one_file_named_by_what_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let (root, cache) = (dir.path(), dir.path().join("cache"));
        assert_eq!(resolve(&Grade::default(), root, &cache).unwrap(), None, "a neutral grade is no file");

        let warm = Grade { temperature: 0.4, ..Grade::default() };
        let a = resolve(&warm, root, &cache).unwrap().unwrap();
        assert!(a.starts_with(cache.join("luts")) && a.is_file());
        assert_eq!(resolve(&warm, root, &cache).unwrap().unwrap(), a, "the same grade, the same file");
        let cool = Grade { temperature: -0.4, ..Grade::default() };
        assert_ne!(resolve(&cool, root, &cache).unwrap().unwrap(), a);

        // A LUT it reads is part of what it is: change the file, and it is a new grade.
        std::fs::write(root.join("look.cube"), "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
        let looked = Grade { look: Some("look.cube".into()), gain: Rgb { r: 0.1, g: 0.0, b: 0.0 }, ..Grade::default() };
        let before = resolve(&looked, root, &cache).unwrap().unwrap();
        std::fs::write(root.join("look.cube"), "LUT_1D_SIZE 2\n0 0 0\n0.9 0.9 0.9\n").unwrap();
        assert_ne!(resolve(&looked, root, &cache).unwrap().unwrap(), before);
    }

    #[test]
    fn a_missing_or_broken_lut_names_itself() {
        let dir = tempfile::tempdir().unwrap();
        let g = Grade { input_lut: Some("log.cube".into()), ..Grade::default() };
        let e = resolve(&g, dir.path(), &dir.path().join("cache")).unwrap_err();
        assert!(e.to_string().contains("log.cube"), "{e}");
        std::fs::write(dir.path().join("log.cube"), "LUT_3D_SIZE 2\n0 0 0\n").unwrap();
        let e = resolve(&g, dir.path(), &dir.path().join("cache")).unwrap_err();
        assert!(e.to_string().contains("log.cube") && e.to_string().contains("needs 8 rows"), "{e}");
    }
}
