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
//! The starter set's curves and primaries, and the tone map and gamut compression here, are
//! FilmCraft's (`crates/color/src/{log,spaces,transform}.rs`), themselves from the makers'
//! published specifications, used under the MIT licence:
//!
//! > Copyright (c) 2026 ArtCraft Team and the FilmCraft contributors
//! >
//! > Permission is hereby granted, free of charge, to any person obtaining a copy of this
//! > software and associated documentation files (the "Software"), to deal in the Software
//! > without restriction, including without limitation the rights to use, copy, modify, merge,
//! > publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons
//! > to whom the Software is furnished to do so, subject to the following conditions:
//! >
//! > The above copyright notice and this permission notice shall be included in all copies or
//! > substantial portions of the Software.
//! >
//! > THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
//! > INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
//! > PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE
//! > FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
//! > OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! > DEALINGS IN THE SOFTWARE.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::grade::encode;

/// A log file as written: a curve and a gamut, or a LUT.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogFile {
    name: String,
    /// Where the numbers come from: the maker's document.
    #[serde(default)]
    #[allow(dead_code)]
    source: String,
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
            let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
            if let Ok(text) = std::fs::read(&path) {
                if let Ok(file) = serde_json::from_slice::<LogFile>(&text) {
                    found.insert(id, file.name);
                }
            }
        }
    }
    found.into_iter().collect()
}

/// The log in the file at `path`, read once and remembered as long as the file does not
/// change (`grade::remembered`) — or why it will not read.
pub fn load(path: &Path) -> Result<Log, String> {
    crate::grade::remembered(path, |p| parse(&std::fs::read(p).map_err(|e| e.to_string())?, p.parent().unwrap_or(Path::new("."))))
}

/// What a log is made of, for naming what it compiles to: its file's bytes, and its LUT's
/// when it is one.
pub fn bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if let Log::Lut(cube) = parse(&bytes, path.parent().unwrap_or(Path::new(".")))? {
        bytes.extend(std::fs::read(&cube).map_err(|e| format!("{}: {e}", cube.display()))?);
    }
    Ok(bytes)
}

/// A log file's bytes, read, its LUT (when it names one) resolved against `folder`.
pub fn parse(bytes: &[u8], folder: &Path) -> Result<Log, String> {
    let file: LogFile = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some(lut) = &file.lut {
        if !file.decode.is_empty() || file.primaries.is_some() {
            return Err("a log is a `lut`, or a `decode` curve with its `primaries`, not both".into());
        }
        if !crate::validate::stays_inside(lut) {
            return Err(format!("`lut` `{lut}` must be relative and stay beside the log"));
        }
        return Ok(Log::Lut(folder.join(lut)));
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

const BT709: [[f64; 2]; 3] = [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060]];
const D65: [f64; 2] = [0.3127, 0.3290];

/// BT.2408's HDR reference white, which scene light 1.0 is taken to be for the tone map.
const REFERENCE_WHITE_NITS: f64 = 203.0;
/// The most a log curve's peak is taken to be, so the knee starts above mid grey.
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
        let mut c = Conversion { signal, pieces, matrix, tone: ToneMap::new(REFERENCE_WHITE_NITS) };
        // The light of the top of the signal, code 1023: what the tone map rolls off from.
        let top = c.decode(match signal {
            Signal::Code => 1.0,
            Signal::Ire => (1023.0 - 64.0) / 876.0,
        });
        c.tone = ToneMap::new((top * REFERENCE_WHITE_NITS).clamp(REFERENCE_WHITE_NITS, LOG_PEAK_NITS));
        c
    }

    /// The scene light the curve's signal `y` stands for.
    fn decode(&self, y: f64) -> f64 {
        self.pieces.iter().find(|p| p.holds(y)).map_or(0.0, |p| p.decode(y))
    }

    /// The curve's signal for a display value, which is the limited-range signal expanded:
    /// 0 is code 64 and 1 is code 940.
    fn signal(&self, v: f64) -> f64 {
        match self.signal {
            Signal::Code => (64.0 + 876.0 * v) / 1023.0,
            Signal::Ire => v,
        }
    }

    /// The display-encoded Rec. 709 colour the log-encoded `c` stands for.
    pub fn apply(&self, c: [f64; 3]) -> [f64; 3] {
        let light = c.map(|v| self.decode(self.signal(v)));
        let light = mul_vec(&self.matrix, light);
        let light = self.tone.apply(light);
        gamut_compress(light).map(|v| encode(v.clamp(0.0, 1.0)))
    }
}

type Mat3 = [[f64; 3]; 3];

/// RGB → XYZ for primaries `p` and white `w` (SMPTE RP 177).
fn npm(p: [[f64; 2]; 3], w: [f64; 2]) -> Mat3 {
    let xyz = |c: [f64; 2]| [c[0] / c[1], 1.0, (1.0 - c[0] - c[1]) / c[1]];
    let (r, g, b) = (xyz(p[0]), xyz(p[1]), xyz(p[2]));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = mul_vec(&inverse(&m), xyz(w));
    std::array::from_fn(|i| std::array::from_fn(|j| m[i][j] * s[j]))
}

fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn mul_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn inverse(m: &Mat3) -> Mat3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let k = 1.0 / (a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g));
    [
        [(e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k],
        [(f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k],
        [(d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k],
    ]
}

/// SMPTE ST 2084's constants.
const PQ_M1: f64 = 0.159_301_758_125;
const PQ_M2: f64 = 78.84375;
const PQ_C1: f64 = 0.8359375;
const PQ_C2: f64 = 18.8515625;
const PQ_C3: f64 = 18.6875;

/// SMPTE ST 2084: a PQ signal's light, as a fraction of 10 000 nits, and back.
fn pq_eotf(e: f64) -> f64 {
    let p = e.max(0.0).powf(1.0 / PQ_M2);
    ((p - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * p)).powf(1.0 / PQ_M1)
}

fn pq_inverse_eotf(y: f64) -> f64 {
    let p = y.max(0.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * p) / (1.0 + PQ_C3 * p)).powf(PQ_M2)
}

/// A PQ signal for `nits`.
fn pq(nits: f64) -> f64 {
    pq_inverse_eotf((nits / 10_000.0).clamp(0.0, 1.0))
}

/// Scene light peaking at `peak` nits onto SDR white: ITU-R BT.2390's EETF (a Hermite knee
/// in the PQ domain) on max(R, G, B), as a ratio. Its knee is the peak's, worked out once.
#[derive(Debug, Clone)]
struct ToneMap {
    peak: f64,
    /// The peak as a PQ signal, white as a fraction of it, and where the knee starts.
    top: f64,
    most: f64,
    ks: f64,
}

impl ToneMap {
    fn new(peak: f64) -> Self {
        let top = pq(peak);
        let most = pq(REFERENCE_WHITE_NITS) / top;
        ToneMap { peak, top, most, ks: 1.5 * most - 0.5 }
    }

    fn apply(&self, c: [f64; 3]) -> [f64; 3] {
        let m = c[0].max(c[1]).max(c[2]);
        if m <= 0.0 {
            return c;
        }
        let nits = m * REFERENCE_WHITE_NITS;
        let k = if nits >= self.peak { 1.0 / m } else { self.eetf(nits) / nits };
        c.map(|v| v * k)
    }

    /// `nits` from a display peaking at `peak` onto one peaking at white.
    fn eetf(&self, nits: f64) -> f64 {
        if REFERENCE_WHITE_NITS >= self.peak {
            return nits.max(0.0).min(self.peak.max(REFERENCE_WHITE_NITS));
        }
        let (top, most, ks) = (self.top, self.most, self.ks);
        let e1 = (pq(nits) / top).min(1.0);
        let e2 = if e1 < ks {
            e1
        } else {
            let t = (e1 - ks) / (1.0 - ks);
            let (t2, t3) = (t * t, t * t * t);
            (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks) + (-2.0 * t3 + 3.0 * t2) * most
        };
        pq_eotf(e2 * top) * 10_000.0
    }
}

/// Colours outside the gamut (a channel below 0 after the matrix) pulled towards grey, after
/// the ACES Reference Gamut Compression (threshold 0.8, limit 1.25, power 1.2): those inside
/// 80% of the boundary keep their values.
fn gamut_compress(c: [f64; 3]) -> [f64; 3] {
    const TH: f64 = 0.8;
    const LIM: f64 = 1.25;
    const P: f64 = 1.2;
    let ach = c[0].max(c[1]).max(c[2]);
    if ach <= 1e-9 {
        return c.map(|v| v.max(0.0));
    }
    let s = (LIM - TH) / (((LIM - TH) / (1.0 - TH)).powf(P) - 1.0).powf(1.0 / P);
    c.map(|v| {
        let d = (ach - v) / ach;
        if d < TH {
            return v;
        }
        let x = (d - TH) / s;
        ach - (TH + s * x / (1.0 + x.powf(P)).powf(1.0 / P)) * ach
    })
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

    /// The makers' published points: a curve that decodes these to their reflectances
    /// reads the footage as the camera wrote it (FilmCraft's own reference values).
    #[test]
    fn each_curve_decodes_its_makers_published_points() {
        let code = |id: &str, cv: f64| curve(id).decode(cv / 1023.0);
        let ire = |id: &str, pct: f64| curve(id).decode(pct / 100.0);
        let close = |got: f64, want: f64, tol: f64, what: &str| assert!((got - want).abs() <= tol, "{what}: {got} not {want}");
        close(code("s-log3", 95.0), 0.0, 1e-9, "S-Log3 black");
        close(code("s-log3", 420.0), 0.18, 1e-9, "S-Log3 grey");
        close(code("v-log", 433.0), 0.18, 0.002, "V-Log grey");
        close(code("logc3", 400.0), 0.18, 0.002, "LogC3 grey");
        close(curve("logc4").decode(0.2784), 0.18, 0.001, "LogC4 grey");
        close(ire("c-log3", 34.3), 0.18, 0.003, "Canon Log 3 grey");
        close(curve("apple-log").decode(0.488), 0.18, 0.001, "Apple Log grey");
        close(curve("d-log").decode(0.398), 0.18, 0.003, "D-Log grey");
        close(code("f-log", 470.0), 0.18, 0.002, "F-Log grey");
        close(code("f-log2", 400.0), 0.18, 0.002, "F-Log2 grey");
        // Every curve rises.
        for (id, _) in list(&starter()) {
            let c = curve(&id);
            let mut last = f64::NEG_INFINITY;
            for i in 0..=100 {
                let v = c.decode(i as f64 / 100.0);
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
        let c = [0.55, 0.42, 0.33];
        // Worked out apart from this code, from FilmCraft's formulas.
        let want = [
            ("s-log3-cine", [0.788968430923, 0.40427914021, 0.300270827988]),
            ("v-log", [0.793945843013, 0.371598961737, 0.301356151667]),
            ("apple-log", [0.677229665181, 0.323861309216, 0.261714563666]),
        ];
        for (id, w) in want {
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
            let e = parse(text.as_bytes(), &project).unwrap_err();
            assert!(e.contains(says), "{text}: {e}");
        }
    }
}
