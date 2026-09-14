//! Wipe patterns. Distro MLT packages often ship no luma files, so we generate the
//! basic set as 8-bit PGM gradients on demand.

use std::path::{Path, PathBuf};

use kuma_core::LUMAS;

/// Resolve a luma name or project-relative path to a file, generating built-ins under
/// `cache_dir`.
pub fn resolve(name: &str, root: &Path, cache_dir: &Path) -> std::io::Result<PathBuf> {
    let p = root.join(name);
    if p.is_file() {
        return Ok(p);
    }
    if LUMAS.contains(&name) {
        let out = cache_dir.join(format!("{name}.pgm"));
        if !out.is_file() {
            std::fs::create_dir_all(cache_dir)?;
            std::fs::write(&out, generate(name, 512, 288))?;
        }
        return Ok(out);
    }
    Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("unknown luma `{name}`")))
}

/// The luma transition reveals B where the map is dark first, so a left wipe is a
/// gradient that is 0 at the left edge and 255 at the right.
pub fn generate(name: &str, w: usize, h: usize) -> Vec<u8> {
    let mut out = format!("P5\n{w} {h}\n255\n").into_bytes();
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let rmax = (cx * cx + cy * cy).sqrt();
    for y in 0..h {
        for x in 0..w {
            let fx = x as f64 / (w as f64 - 1.0);
            let fy = y as f64 / (h as f64 - 1.0);
            let v = match name {
                "wipe_left" => fx,
                "wipe_right" => 1.0 - fx,
                "wipe_down" => fy,
                "wipe_up" => 1.0 - fy,
                "diagonal" => (fx + fy) / 2.0,
                "iris" => {
                    let dx = x as f64 - cx;
                    let dy = y as f64 - cy;
                    (dx * dx + dy * dy).sqrt() / rmax
                }
                _ => fx,
            };
            out.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_pgm() {
        let pgm = generate("wipe_left", 8, 2);
        assert!(pgm.starts_with(b"P5\n8 2\n255\n"));
        let px = &pgm[b"P5\n8 2\n255\n".len()..];
        assert_eq!(px.len(), 16);
        assert_eq!(px[0], 0);
        assert_eq!(px[7], 255);
    }
}
