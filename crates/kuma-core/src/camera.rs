//! Camera log footage into Rec. 709: a grade's `log` step, computed from the camera maker's
//! published curve rather than read from their `.cube`, so footage converts without a file
//! to find first.
//!
//! A camera log is a file, not code: `logs/<id>.json` in a library or the project (the
//! project's own wins, as with fonts), which the grade names by id. The file holds the curve
//! that decodes the signal to scene light — pieces of the shapes makers publish (linear, log,
//! power, constant), each up to a bound — the primaries of the camera's gamut, and where the
//! numbers come from:
//!
//! ```json
//! { "name": "Sony S-Log3 / S-Gamut3.Cine", "source": "Sony, Technical Summary …",
//!   "signal": "code",
//!   "decode": [{ "below": 0.1673, "linear": { "slope": 6.62, "offset": 0.0928 } },
//!              { "log": { "a": 5.26, "b": 0.0526, "c": 0.2556, "d": 0.4105, "base": 10 } }],
//!   "primaries": [[0.766, 0.275], [0.225, 0.8], [0.089, -0.087]] }
//! ```
//!
//! — or, for a maker who publishes only a LUT, `{ "name": …, "lut": "file.cube" }`, the
//! `.cube` beside the file: that LUT is then the grade's input. The starter set
//! (`library/logs/`) holds the curves below.
//!
//! A frame reaches the grade as display R'G'B' from 0 to 1 — the camera's limited-range
//! signal, expanded. Each channel is decoded with the camera's published curve to scene light
//! (18% grey is 0.18), the camera's gamut is turned into Rec. 709's primaries, the light is
//! tone mapped to SDR white with ITU-R BT.2390's EETF on max(R, G, B) as a ratio (so hue and
//! saturation keep), colours the wider gamut put outside Rec. 709 are pulled in towards grey,
//! and the result is display-encoded for BT.1886. Mid grey lands on mid grey; the highlights
//! a camera holds above white roll off into it instead of clipping.
//!
//! Like the rest of the grade this is **append-only in behaviour**: footage converts the same
//! every time it compiles, so a constant here — or a number in a starter log — changing
//! re-grades finished videos. A log file's bytes are part of the name of the LUT it compiles
//! to, so an edited one is a new grade, as an edited `.cube` is.
//!
//! Where it all comes from, so each number can be checked against its source: every curve
//! and gamut in the starter set is the camera maker's own published document, named in the
//! file's `source`; PQ is SMPTE ST 2084, the tone map ITU-R BT.2390's EETF, white BT.2408's
//! reference white, the gamut matrix SMPTE RP 177's derivation. How a colour outside
//! Rec. 709 is brought in is kuma's own: towards the grey of its luminance, just far enough.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::grade::encode;

/// A log file as written: a curve and a gamut, or a LUT.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogFile {
    name: String,
    /// Where the numbers come from: the maker's document. For people; nothing reads it.
    #[serde(default, rename = "source")]
    _source: String,
    #[serde(default)]
    signal: Signal,
    #[serde(default)]
    decode: Vec<Piece>,
    primaries: Option<[[f64; 2]; 3]>,
    /// The gamut's white, D65 when left out.
    white: Option<[f64; 2]>,
    /// A `.cube` beside this file, instead of a curve and a gamut.
    lut: Option<String>,
}

/// How a curve's formula reads the signal.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Signal {
    /// 10-bit code values over 1023, as Sony, Panasonic, ARRI, DJI and Fujifilm write theirs.
    #[default]
    Code,
    /// IRE, code 64 as 0 and 940 as 1, as Canon and Apple write theirs.
    Ire,
}

impl Signal {
    /// The 10-bit code value `code` as the curve reads it.
    fn of_code(self, code: f64) -> f64 {
        match self {
            Signal::Code => code / 1023.0,
            Signal::Ire => (code - 64.0) / 876.0,
        }
    }
}

/// One piece of a curve, for signals below (or up to) its bound; the last has none.
#[derive(Debug, Clone, Deserialize)]
struct Piece {
    below: Option<f64>,
    upto: Option<f64>,
    #[serde(flatten)]
    shape: Shape,
}

/// What a piece makes of a signal `y`, written as the makers write the encoding.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Shape {
    /// `y = slope·x + offset`.
    Linear { slope: f64, offset: f64 },
    /// `y = c·log_base(a·x + b) + d`.
    Log {
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        #[serde(default = "ten")]
        base: f64,
    },
    /// `y = scale·(x − from)^power`.
    Power { scale: f64, power: f64, from: f64 },
    /// `x = constant`, whatever the signal.
    Constant(f64),
}

fn ten() -> f64 {
    10.0
}

/// The scene light the signal `y` stands for: the first piece that holds it decodes it.
fn decode(pieces: &[Piece], y: f64) -> f64 {
    pieces.iter().find(|p| p.holds(y)).map_or(0.0, |p| p.decode(y))
}

impl Piece {
    fn holds(&self, y: f64) -> bool {
        self.below.is_none_or(|b| y < b) && self.upto.is_none_or(|u| y <= u)
    }

    fn decode(&self, y: f64) -> f64 {
        match self.shape {
            Shape::Linear { slope, offset } => (y - offset) / slope,
            Shape::Log { a, b, c, d, base } => (base.powf((y - d) / c) - b) / a,
            Shape::Power { scale, power, from } => (y / scale).powf(1.0 / power) + from,
            Shape::Constant(x) => x,
        }
    }
}

/// A camera log, read: a curve to convert with, or the LUT the maker gives instead.
#[derive(Debug, Clone)]
pub enum Log {
    Curve(Arc<Conversion>),
    /// The `.cube` to take as the grade's input.
    Lut(PathBuf),
}

/// The file the log `id` is in: `<dir>/<id>.json` in the last of `dirs` that has one — the
/// project's own `logs/` last, so it wins.
pub fn find(id: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().rev().map(|d| d.join(format!("{id}.json"))).find(|p| p.is_file())
}

/// Every log in `dirs`, by id, with its name: what the editor offers. Later directories win.
pub fn list(dirs: &[PathBuf]) -> Vec<(String, String)> {
    let mut found = std::collections::BTreeMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
            let file = std::fs::read(&path).ok().and_then(|text| serde_json::from_slice::<LogFile>(&text).ok());
            if let (Some(id), Some(file)) = (path.file_stem(), file) {
                found.insert(id.to_string_lossy().into_owned(), file.name);
            }
        }
    }
    found.into_iter().collect()
}

/// The log in the file at `path`, read once and remembered as long as the file does not
/// change (`grade::remembered`) — or why it will not read.
pub fn load(path: &Path) -> Result<Log, String> {
    crate::grade::remembered(path, |p| parse(&std::fs::read(p).map_err(|e| e.to_string())?, p))
}

/// What a log is made of, for naming what it compiles to: its file's bytes, and its LUT's
/// when it is one.
pub fn bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if let Log::Lut(cube) = parse(&bytes, path)? {
        bytes.extend(std::fs::read(&cube).map_err(|e| format!("{}: {e}", cube.display()))?);
    }
    Ok(bytes)
}

/// The bytes of the log file at `path`, read; its LUT, when it names one, is beside it.
pub fn parse(bytes: &[u8], path: &Path) -> Result<Log, String> {
    let file: LogFile = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some(lut) = &file.lut {
        if !file.decode.is_empty() || file.primaries.is_some() {
            return Err("a log is a `lut`, or a `decode` curve with its `primaries`, not both".into());
        }
        if !crate::validate::stays_inside(lut) {
            return Err(format!("`lut` `{lut}` must be relative and stay beside the log"));
        }
        return Ok(Log::Lut(path.with_file_name(lut)));
    }
    let primaries = file.primaries.ok_or("a log needs the `primaries` of its gamut")?;
    let Some((last, rest)) = file.decode.split_last() else { return Err("a log needs a `decode` curve".into()) };
    if last.below.is_some() || last.upto.is_some() {
        return Err("the last piece of `decode` takes every signal past the others: give it no bound".into());
    }
    let mut bound = f64::NEG_INFINITY;
    for (i, p) in rest.iter().enumerate() {
        let b = match (p.below, p.upto) {
            (Some(b), None) | (None, Some(b)) => b,
            _ => return Err(format!("`decode` piece {i}: one of `below` or `upto`")),
        };
        if b <= bound {
            return Err(format!("`decode` piece {i}: bounds must rise"));
        }
        bound = b;
    }
    let well_formed = file.decode.iter().all(|p| match p.shape {
        Shape::Linear { slope, offset } => slope != 0.0 && offset.is_finite(),
        Shape::Log { a, b, c, d, base } => a != 0.0 && c != 0.0 && base > 0.0 && base != 1.0 && b.is_finite() && d.is_finite(),
        Shape::Power { scale, power, from } => scale != 0.0 && power != 0.0 && from.is_finite(),
        Shape::Constant(x) => x.is_finite(),
    });
    if !well_formed {
        return Err("a `decode` piece has a number that divides by nothing or is not finite".into());
    }
    Ok(Log::Curve(Arc::new(Conversion::new(file.signal, file.decode, primaries, file.white.unwrap_or(D65)))))
}

/// Rec. 709's primaries and D65, its white (ITU-R BT.709).
const BT709: [[f64; 2]; 3] = [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060]];
const D65: [f64; 2] = [0.3127, 0.3290];

/// Scene light 1.0 as display light: ITU-R BT.2408's HDR reference white, 203 cd/m².
const REFERENCE_WHITE_NITS: f64 = 203.0;
/// The brightest a curve's top is taken to be. A log curve's top is often tens of times
/// white; taken whole, BT.2390's knee would start below mid grey and darken the middle of
/// the picture. Capped here, the knee starts above mid grey for every curve (at 4000 cd/m²
/// it starts at 0.46 of the range in PQ, mid grey being 0.43), and what is brighter still
/// is white.
const LOG_PEAK_NITS: f64 = 4000.0;

/// A camera log's curve, ready to convert colours with: its gamut matrix and tone map
/// worked out once.
#[derive(Debug, Clone)]
pub struct Conversion {
    signal: Signal,
    pieces: Vec<Piece>,
    matrix: Mat3,
    tone: ToneMap,
}

impl Conversion {
    fn new(signal: Signal, pieces: Vec<Piece>, primaries: [[f64; 2]; 3], white: [f64; 2]) -> Self {
        let matrix = mul(&inverse(&npm(BT709, D65)), &npm(primaries, white));
        // The light of the top of the signal, code 1023: what the tone map rolls off from.
        let top = decode(&pieces, signal.of_code(1023.0));
        let tone = ToneMap::new((top * REFERENCE_WHITE_NITS).clamp(REFERENCE_WHITE_NITS, LOG_PEAK_NITS));
        Conversion { signal, pieces, matrix, tone }
    }

    /// The display-encoded Rec. 709 colour the log-encoded `c` stands for. A display value
    /// is the camera's limited-range signal expanded: 0 is code 64 and 1 is code 940.
    pub fn apply(&self, c: [f64; 3]) -> [f64; 3] {
        let light = c.map(|v| decode(&self.pieces, self.signal.of_code(64.0 + 876.0 * v)));
        let light = mul_vec(&self.matrix, light);
        let light = self.tone.apply(light);
        into_gamut(light).map(|v| encode(v.clamp(0.0, 1.0)))
    }
}

type Mat3 = [[f64; 3]; 3];

/// The matrix taking linear RGB with `primaries` and `white` to CIE XYZ, white at Y = 1:
/// SMPTE RP 177's normalised primary matrix — the primaries' XYZ as columns, each scaled so
/// the three add up to the white.
fn npm(primaries: [[f64; 2]; 3], white: [f64; 2]) -> Mat3 {
    let xyz = |[x, y]: [f64; 2]| [x / y, 1.0, (1.0 - x - y) / y];
    let columns = primaries.map(xyz);
    let p: Mat3 = std::array::from_fn(|row| std::array::from_fn(|col| columns[col][row]));
    let scale = mul_vec(&inverse(&p), xyz(white));
    std::array::from_fn(|row| std::array::from_fn(|col| p[row][col] * scale[col]))
}

fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    std::array::from_fn(|row| std::array::from_fn(|col| (0..3).map(|k| a[row][k] * b[k][col]).sum()))
}

fn mul_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|row| (0..3).map(|k| m[row][k] * v[k]).sum())
}

/// The inverse of `m`: its adjugate over its determinant.
fn inverse(m: &Mat3) -> Mat3 {
    // Cofactor of row r, column c, with the rows and columns after them wrapping round,
    // which takes care of the signs.
    let cofactor = |r: usize, c: usize| {
        let (r1, r2, c1, c2) = ((r + 1) % 3, (r + 2) % 3, (c + 1) % 3, (c + 2) % 3);
        m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
    };
    let det: f64 = (0..3).map(|c| m[0][c] * cofactor(0, c)).sum();
    std::array::from_fn(|row| std::array::from_fn(|col| cofactor(col, row) / det))
}

/// SMPTE ST 2084's (PQ) constants.
const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

/// ST 2084's signal for `nits` of display light, 0 to 10 000.
fn pq(nits: f64) -> f64 {
    let y = (nits / 10_000.0).clamp(0.0, 1.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
}

/// ST 2084's display light, in nits, for the signal `e`.
fn pq_nits(e: f64) -> f64 {
    let p = e.max(0.0).powf(1.0 / PQ_M2);
    10_000.0 * ((p - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * p)).powf(1.0 / PQ_M1)
}

/// Light up to `peak` nits brought down to white: the EETF of ITU-R BT.2390 (§5.4), worked
/// in PQ, from a source peaking at `peak` to a display peaking at reference white, with no
/// black lift. It is applied to the brightest of a colour's three channels and the others
/// scaled with it, as SMPTE ST 2094-40 tone maps (`maxRGB`), so a colour keeps its hue and
/// saturation as it is brought down.
#[derive(Debug, Clone)]
struct ToneMap {
    peak: f64,
    /// The source peak in PQ; the target's, as a fraction of it (BT.2390's `maxLum`); and
    /// where the knee starts (`KS`).
    source: f64,
    max_lum: f64,
    knee: f64,
}

impl ToneMap {
    fn new(peak: f64) -> Self {
        let source = pq(peak);
        let max_lum = pq(REFERENCE_WHITE_NITS) / source;
        ToneMap { peak, source, max_lum, knee: 1.5 * max_lum - 0.5 }
    }

    fn apply(&self, c: [f64; 3]) -> [f64; 3] {
        let brightest = c[0].max(c[1]).max(c[2]);
        if brightest <= 0.0 {
            return c;
        }
        let nits = brightest * REFERENCE_WHITE_NITS;
        let k = if nits >= self.peak { 1.0 / brightest } else { self.eetf(nits) / nits };
        c.map(|v| v * k)
    }

    /// BT.2390's curve: unchanged below the knee, then a Hermite spline from the knee to
    /// the target's peak.
    fn eetf(&self, nits: f64) -> f64 {
        // A source no brighter than white has nothing to bring down.
        if self.max_lum >= 1.0 {
            return nits;
        }
        let e1 = (pq(nits) / self.source).min(1.0);
        let e2 = if e1 < self.knee {
            e1
        } else {
            let t = (e1 - self.knee) / (1.0 - self.knee);
            let (t2, t3) = (t * t, t * t * t);
            (2.0 * t3 - 3.0 * t2 + 1.0) * self.knee + (t3 - 2.0 * t2 + t) * (1.0 - self.knee) + (-2.0 * t3 + 3.0 * t2) * self.max_lum
        };
        pq_nits(e2 * self.source)
    }
}

/// A colour the camera's wider gamut puts outside Rec. 709 — a channel below nothing once
/// it is Rec. 709's — moved towards the grey of its own luminance just far enough to come
/// inside, keeping its luminance and hue. A colour already inside is left as it is.
fn into_gamut(c: [f64; 3]) -> [f64; 3] {
    let least = c[0].min(c[1]).min(c[2]);
    if least >= 0.0 {
        return c;
    }
    let y = crate::grade::luma(c);
    if y <= 0.0 {
        return [0.0; 3];
    }
    let keep = y / (y - least);
    c.map(|v| y + (v - y) * keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The starter set, where it ships.
    fn starter() -> Vec<PathBuf> {
        vec![PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../library/logs"))]
    }

    fn curve(id: &str) -> Arc<Conversion> {
        match load(&find(id, &starter()).unwrap_or_else(|| panic!("no `{id}`"))).unwrap() {
            Log::Curve(c) => c,
            Log::Lut(_) => panic!("`{id}` is a curve"),
        }
    }

    /// The makers' published points — the code values their documents give for black, 18%
    /// grey and 90% white — decode to those reflectances.
    #[test]
    fn each_curve_decodes_its_makers_published_points() {
        let code = |id: &str, cv: f64| decode(&curve(id).pieces, cv / 1023.0);
        let ire = |id: &str, pct: f64| decode(&curve(id).pieces, pct / 100.0);
        let close = |got: f64, want: f64, tol: f64, what: &str| assert!((got - want).abs() <= tol, "{what}: {got} not {want}");
        close(code("s-log3", 95.0), 0.0, 1e-9, "S-Log3 black");
        close(code("s-log3", 420.0), 0.18, 1e-9, "S-Log3 grey");
        close(code("v-log", 433.0), 0.18, 0.002, "V-Log grey");
        close(code("logc3", 400.0), 0.18, 0.002, "LogC3 grey");
        close(decode(&curve("logc4").pieces, 0.2784), 0.18, 0.001, "LogC4 grey");
        close(ire("c-log3", 34.3), 0.18, 0.003, "Canon Log 3 grey");
        close(decode(&curve("apple-log").pieces, 0.488), 0.18, 0.001, "Apple Log grey");
        close(decode(&curve("d-log").pieces, 0.398), 0.18, 0.003, "D-Log grey");
        close(code("f-log", 470.0), 0.18, 0.002, "F-Log grey");
        close(code("f-log2", 400.0), 0.18, 0.002, "F-Log2 grey");
        // Every curve rises.
        for (id, _) in list(&starter()) {
            let c = curve(&id);
            let mut last = f64::NEG_INFINITY;
            for i in 0..=100 {
                let v = decode(&c.pieces, i as f64 / 100.0);
                assert!(v > last, "{id} at {i}");
                last = v;
            }
        }
    }

    #[test]
    fn mid_grey_comes_out_mid_grey_and_white_rolls_off() {
        let grey = encode(0.18);
        // S-Log3's grey is code 420, which is (420 − 64) / 876 once expanded.
        let s = curve("s-log3-cine");
        let out = s.apply([(420.0 - 64.0) / 876.0; 3]);
        assert!(out.iter().all(|c| (c - grey).abs() < 1e-6), "{out:?} not {grey}");
        // A neutral stays neutral through the gamut matrix, whatever the camera.
        let all = list(&starter());
        assert_eq!(all.len(), 12, "{all:?}");
        for (id, _) in all {
            let out = curve(&id).apply([0.6; 3]);
            assert!((out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6, "{id}: {out:?}");
        }
        // 90% white sits under white, and the top of the signal reaches it.
        let white = s.apply([(598.0 - 64.0) / 876.0; 3])[0];
        assert!(white > grey && white < 1.0, "{white}");
        assert!((s.apply([1.0; 3])[0] - 1.0).abs() < 1e-3);
    }

    /// The values footage has always converted to. If one has to change, the change
    /// re-grades every video shot in that log.
    #[test]
    fn each_log_keeps_the_values_it_has_always_given() {
        // Worked out apart from this code, from the files and the standards.
        let want = [
            ("s-log3-cine", [0.55, 0.42, 0.33], [0.788968430923, 0.40427914021, 0.255912754195]),
            ("v-log", [0.55, 0.42, 0.33], [0.793945843013, 0.368742250574, 0.255687751465]),
            ("apple-log", [0.55, 0.42, 0.33], [0.677229665181, 0.322539190092, 0.228363462642]),
            // A red past Rec. 709's: brought in until green is nothing.
            ("s-log3-cine", [0.7, 0.3, 0.25], [0.825397156196, 0.0, 0.258730375702]),
        ];
        for (id, c, w) in want {
            let got = curve(id).apply(c);
            assert!((0..3).all(|i| (got[i] - w[i]).abs() < 1e-8), "{id}: {got:?} not {w:?}");
        }
    }

    /// A log is found where it is, the project's own winning; one a maker gives as a LUT is
    /// that LUT; and a file that cannot be a curve says why.
    #[test]
    fn a_log_is_a_file_found_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("logs");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("d-log-m.json"), r#"{ "name": "DJI D-Log M", "lut": "dlogm.cube" }"#).unwrap();
        std::fs::write(project.join("s-log3.json"), r#"{ "name": "Mine", "lut": "mine.cube" }"#).unwrap();
        let dirs = [starter(), vec![project.clone()]].concat();
        assert!(matches!(load(&find("d-log-m", &dirs).unwrap()).unwrap(), Log::Lut(p) if p == project.join("dlogm.cube")));
        assert_eq!(find("s-log3", &dirs).unwrap(), project.join("s-log3.json"), "the project's own wins");
        assert!(list(&dirs).contains(&("d-log-m".into(), "DJI D-Log M".into())));
        assert!(find("no-such-log", &dirs).is_none());
        for (text, says) in [
            (r#"{ "name": "x", "decode": [{ "log": { "a": 1, "b": 0, "c": 1, "d": 0 } }] }"#, "primaries"),
            (r#"{ "name": "x", "primaries": [[0,0],[0,1],[1,0]], "decode": [{ "below": 0.1, "linear": { "slope": 1, "offset": 0 } }] }"#, "no bound"),
            (r#"{ "name": "x", "primaries": [[0,0],[0,1],[1,0]], "decode": [{ "below": 0.2, "constant": 0 }, { "below": 0.1, "constant": 0 }, { "constant": 1 }] }"#, "rise"),
            (r#"{ "name": "x", "primaries": [[0,0],[0,1],[1,0]], "decode": [{ "linear": { "slope": 0, "offset": 0 } }] }"#, "divides by nothing"),
            (r#"{ "name": "x", "lut": "../elsewhere.cube" }"#, "stay beside"),
        ] {
            let e = parse(text.as_bytes(), &project.join("x.json")).unwrap_err();
            assert!(e.contains(says), "{text}: {e}");
        }
    }
}
