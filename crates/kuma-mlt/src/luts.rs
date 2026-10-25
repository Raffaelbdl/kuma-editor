//! Grades, as the LUT files MLT applies. A [`Grade`] is computed by `kuma_core::grade` and
//! written as one `.cube` under the cache, named by a hash of everything it is made of —
//! its settings and the bytes of the LUTs it reads — so the same grade compiles to the
//! same path, a changed one to a new path (which is what tells the preview to reload),
//! and a file is written once however many clips share it. Naming a grade reads its LUTs
//! but parses nothing; they are parsed only to write a file that is not there yet.

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
    let Some((name, luts)) = read(grade, root)? else { return Ok(None) };
    let path = crate::generated::write_once(&dir(cache_dir), &name, || {
        let [input, look] = luts.map(|lut| {
            lut.map(|(path, bytes)| Lut::from_bytes(&bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("LUT `{path}`: {e}"))))
                .transpose()
        });
        Ok(Grading::new(grade, input?, look?).cube().into_bytes())
    })?;
    Ok(Some(path))
}

/// The name of the file `grade` compiles to in [`dir`], or `None` for a grade that changes
/// nothing: what a prune keeps.
pub fn name(grade: &Grade, root: &Path) -> std::io::Result<Option<String>> {
    Ok(read(grade, root)?.map(|(name, _)| name))
}

/// A grade's LUTs as they are on disk (input, then look), with their paths.
type Luts<'g> = [Option<(&'g str, Vec<u8>)>; 2];

/// The file name a grade compiles to, and the LUT bytes that went into it.
fn read<'g>(grade: &'g Grade, root: &Path) -> std::io::Result<Option<(String, Luts<'g>)>> {
    if grade.is_neutral() {
        return Ok(None);
    }
    let mut hash = Sha256::new();
    hash.update(b"kuma grade\n");
    // What it does to a colour, not where: a mask is the compiler's, around the filter.
    let colour = Grade { mask: None, ..grade.clone() };
    hash.update(serde_json::to_vec(&colour).map_err(std::io::Error::other)?);
    let mut luts: Luts = [None, None];
    for (slot, path) in luts.iter_mut().zip([&grade.input_lut, &grade.look]) {
        let Some(path) = path.as_deref() else { continue };
        let bytes = std::fs::read(root.join(path)).map_err(|e| std::io::Error::new(e.kind(), format!("LUT `{path}`: {e}")))?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        *slot = Some((path, bytes));
    }
    Ok(Some((format!("{}.cube", hex::encode(&hash.finalize()[..16])), luts)))
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
        assert_eq!(name(&warm, root).unwrap().as_deref(), a.file_name().and_then(|n| n.to_str()), "named without being written");
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
