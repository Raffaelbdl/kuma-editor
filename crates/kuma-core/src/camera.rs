//! Camera log footage into Rec. 709: a grade's `log` step, computed here rather than read
//! from the camera maker's `.cube`, so footage converts without a file to find first.
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
//! every time it compiles, so a constant here changing re-grades finished videos.
//!
//! The curves, primaries, tone map and gamut compression are FilmCraft's
//! (`crates/color/src/{log,spaces,transform}.rs`), themselves from the makers' published
//! specifications, used under the MIT licence:
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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::grade::encode;

/// A camera's log encoding and the gamut it records in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CameraLog {
    /// Sony S-Log3, S-Gamut3.Cine: what Sony's cameras offer first.
    SLog3Cine,
    /// Sony S-Log3, S-Gamut3.
    SLog3,
    /// Panasonic V-Log, V-Gamut.
    VLog,
    /// Canon Log 2, Cinema Gamut.
    CLog2,
    /// Canon Log 3, Cinema Gamut.
    CLog3,
    /// ARRI LogC3 (EI 800), ARRI Wide Gamut 3.
    Logc3,
    /// ARRI LogC4, ARRI Wide Gamut 4.
    Logc4,
    /// Apple Log (iPhone 15 Pro and later), Rec. 2020 primaries.
    AppleLog,
    /// DJI D-Log, D-Gamut.
    DLog,
    /// Fujifilm F-Log, F-Gamut (Rec. 2020 primaries).
    FLog,
    /// Fujifilm F-Log2, F-Gamut.
    FLog2,
    /// Fujifilm F-Log2 C, F-Gamut C.
    FLog2c,
}

impl CameraLog {
    pub const ALL: [CameraLog; 12] = [
        CameraLog::SLog3Cine,
        CameraLog::SLog3,
        CameraLog::VLog,
        CameraLog::CLog2,
        CameraLog::CLog3,
        CameraLog::Logc3,
        CameraLog::Logc4,
        CameraLog::AppleLog,
        CameraLog::DLog,
        CameraLog::FLog,
        CameraLog::FLog2,
        CameraLog::FLog2c,
    ];

    fn curve(self) -> Curve {
        match self {
            CameraLog::SLog3Cine | CameraLog::SLog3 => Curve::SLog3,
            CameraLog::VLog => Curve::VLog,
            CameraLog::CLog2 => Curve::CLog2,
            CameraLog::CLog3 => Curve::CLog3,
            CameraLog::Logc3 => Curve::LogC3,
            CameraLog::Logc4 => Curve::LogC4,
            CameraLog::AppleLog => Curve::AppleLog,
            CameraLog::DLog => Curve::DLog,
            CameraLog::FLog => Curve::FLog,
            CameraLog::FLog2 | CameraLog::FLog2c => Curve::FLog2,
        }
    }

    /// The gamut's primaries, (x, y) of red, green and blue, all with a D65 white.
    fn primaries(self) -> [[f64; 2]; 3] {
        match self {
            CameraLog::SLog3Cine => [[0.766, 0.275], [0.225, 0.800], [0.089, -0.087]],
            CameraLog::SLog3 => [[0.730, 0.280], [0.140, 0.855], [0.100, -0.050]],
            CameraLog::VLog => [[0.730, 0.280], [0.165, 0.840], [0.100, -0.030]],
            CameraLog::CLog2 | CameraLog::CLog3 => [[0.740, 0.270], [0.170, 1.140], [0.080, -0.100]],
            CameraLog::Logc3 => [[0.6840, 0.3130], [0.2210, 0.8480], [0.0861, -0.1020]],
            CameraLog::Logc4 => [[0.7347, 0.2653], [0.1424, 0.8576], [0.0991, -0.0308]],
            CameraLog::AppleLog | CameraLog::FLog | CameraLog::FLog2 => BT2020,
            CameraLog::DLog => [[0.71, 0.31], [0.21, 0.88], [0.09, -0.08]],
            CameraLog::FLog2c => [[0.7347, 0.2653], [0.0263, 0.9737], [0.1173, -0.0224]],
        }
    }
}

const BT709: [[f64; 2]; 3] = [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060]];
const BT2020: [[f64; 2]; 3] = [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]];
const D65: [f64; 2] = [0.3127, 0.3290];

/// BT.2408's HDR reference white, which scene light 1.0 is taken to be for the tone map.
const REFERENCE_WHITE_NITS: f64 = 203.0;
/// The most a log curve's peak is taken to be, so the knee starts above mid grey.
const LOG_PEAK_NITS: f64 = 4000.0;

/// A camera log, ready to convert colours with: its gamut matrix and tone map worked out once.
#[derive(Debug, Clone)]
pub struct Conversion {
    curve: Curve,
    matrix: Mat3,
    tone: ToneMap,
}

impl Conversion {
    pub fn new(log: CameraLog) -> Self {
        let curve = log.curve();
        let matrix = mul(&inverse(&npm(BT709, D65)), &npm(log.primaries(), D65));
        let peak = (curve.peak_linear() * REFERENCE_WHITE_NITS).clamp(REFERENCE_WHITE_NITS, LOG_PEAK_NITS);
        Conversion { curve, matrix, tone: ToneMap::new(peak) }
    }

    /// The display-encoded Rec. 709 colour the log-encoded `c` stands for.
    pub fn apply(&self, c: [f64; 3]) -> [f64; 3] {
        let light = c.map(|v| self.curve.decode(self.curve.signal(v)));
        let light = mul_vec(&self.matrix, light);
        let light = self.tone.apply(light);
        gamut_compress(light).map(|v| encode(v.clamp(0.0, 1.0)))
    }
}

/// A log curve, `decode(signal) → scene light`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Curve {
    SLog3,
    VLog,
    CLog2,
    CLog3,
    LogC3,
    LogC4,
    AppleLog,
    DLog,
    FLog,
    FLog2,
}

impl Curve {
    /// Whether the curve's formula is written in IRE (code 64 → 0, 940 → 1), as Canon's and
    /// Apple's are; the others are written in 10-bit code values over 1023.
    fn ire(self) -> bool {
        matches!(self, Curve::CLog2 | Curve::CLog3 | Curve::AppleLog)
    }

    /// The curve's signal for a display value, which is the limited-range signal expanded:
    /// 0 is code 64 and 1 is code 940.
    fn signal(self, v: f64) -> f64 {
        if self.ire() { v } else { (64.0 + 876.0 * v) / 1023.0 }
    }

    /// The scene light of the top of the signal, code 1023.
    fn peak_linear(self) -> f64 {
        self.decode(if self.ire() { (1023.0 - 64.0) / 876.0 } else { 1.0 })
    }

    fn decode(self, y: f64) -> f64 {
        match self {
            Curve::SLog3 => {
                if y >= 171.210_294_692_9 / 1023.0 {
                    10f64.powf((y * 1023.0 - 420.0) / 261.5) * (0.18 + 0.01) - 0.01
                } else {
                    (y * 1023.0 - 95.0) * 0.011_25 / (171.210_294_692_9 - 95.0)
                }
            }
            Curve::VLog => {
                if y < 0.181 { (y - 0.125) / 5.6 } else { 10f64.powf((y - 0.598_206) / 0.241_514) - 0.008_73 }
            }
            Curve::CLog2 => {
                let x = if y < 0.092_864_125 {
                    -(10f64.powf((0.092_864_125 - y) / 0.241_360_77) - 1.0) / 87.099_375_46
                } else {
                    (10f64.powf((y - 0.092_864_125) / 0.241_360_77) - 1.0) / 87.099_375_46
                };
                x * 0.9
            }
            Curve::CLog3 => {
                let x = if y < 0.097_465_473 {
                    -(10f64.powf((0.127_839_01 - y) / 0.367_268_45) - 1.0) / 14.983_25
                } else if y <= 0.152_778_91 {
                    (y - 0.125_122_19) / 1.975_479_8
                } else {
                    (10f64.powf((y - 0.122_405_37) / 0.367_268_45) - 1.0) / 14.983_25
                };
                x * 0.9
            }
            Curve::LogC3 => {
                let (cut, a, b, c, d, e, f) = (0.010_591, 5.555_556, 0.052_272, 0.247_190, 0.385_537, 5.367_655, 0.092_809);
                if y > e * cut + f { (10f64.powf((y - d) / c) - b) / a } else { (y - f) / e }
            }
            Curve::LogC4 => {
                let a = (2f64.powi(18) - 16.0) / 117.45;
                let b = (1023.0 - 95.0) / 1023.0;
                let c = 95.0 / 1023.0;
                let s = (7.0 * std::f64::consts::LN_2 * 2f64.powf(7.0 - 14.0 * c / b)) / (a * b);
                let t = (2f64.powf(14.0 * (-c / b) + 6.0) - 64.0) / a;
                if y >= 0.0 { (2f64.powf(14.0 * (y - c) / b + 6.0) - 64.0) / a } else { y * s + t }
            }
            Curve::AppleLog => {
                let (r0, rt, c, beta, gamma, delta) = (-0.056_410_88, 0.01, 47.287_112_36, 0.009_640_52, 0.085_504_79, 0.693_369_45);
                if y >= c * (rt - r0) * (rt - r0) {
                    2f64.powf((y - delta) / gamma) - beta
                } else if y >= 0.0 {
                    (y / c).sqrt() + r0
                } else {
                    r0
                }
            }
            Curve::DLog => {
                if y <= 0.14 { (y - 0.0929) / 6.025 } else { (10f64.powf(3.896_16 * y - 2.277_52) - 0.0108) / 0.9892 }
            }
            Curve::FLog => fujifilm(0.555_556, 0.009_468, 0.344_676, 0.790_453, 8.735_631, 0.092_864, 0.100_537_775_223_865, y),
            Curve::FLog2 => fujifilm(5.555_556, 0.064_829, 0.245_281, 0.384_316, 8.799_461, 0.092_864, 0.100_686_685_370_811, y),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fujifilm(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, cut: f64, y: f64) -> f64 {
    if y >= cut { 10f64.powf((y - d) / c) / a - b / a } else { (y - f) / e }
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

    /// The makers' published points: a curve that decodes these to their reflectances
    /// reads the footage as the camera wrote it (FilmCraft's own reference values).
    #[test]
    fn each_curve_decodes_its_makers_published_points() {
        let code = |c: Curve, cv: f64| c.decode(cv / 1023.0);
        let ire = |c: Curve, pct: f64| c.decode(pct / 100.0);
        let close = |got: f64, want: f64, tol: f64, what: &str| assert!((got - want).abs() <= tol, "{what}: {got} not {want}");
        close(code(Curve::SLog3, 95.0), 0.0, 1e-9, "S-Log3 black");
        close(code(Curve::SLog3, 420.0), 0.18, 1e-9, "S-Log3 grey");
        close(code(Curve::VLog, 433.0), 0.18, 0.002, "V-Log grey");
        close(code(Curve::LogC3, 400.0), 0.18, 0.002, "LogC3 grey");
        close(Curve::LogC4.decode(0.2784), 0.18, 0.001, "LogC4 grey");
        close(ire(Curve::CLog3, 34.3), 0.18, 0.003, "Canon Log 3 grey");
        close(Curve::AppleLog.decode(0.488), 0.18, 0.001, "Apple Log grey");
        close(Curve::DLog.decode(0.398), 0.18, 0.003, "D-Log grey");
        close(code(Curve::FLog, 470.0), 0.18, 0.002, "F-Log grey");
        close(code(Curve::FLog2, 400.0), 0.18, 0.002, "F-Log2 grey");
        // Every curve rises.
        for c in CameraLog::ALL.map(CameraLog::curve) {
            let mut last = f64::NEG_INFINITY;
            for i in 0..=100 {
                let v = c.decode(i as f64 / 100.0);
                assert!(v > last, "{c:?} at {i}");
                last = v;
            }
        }
    }

    #[test]
    fn mid_grey_comes_out_mid_grey_and_white_rolls_off() {
        let grey = encode(0.18);
        // S-Log3's grey is code 420, which is (420 − 64) / 876 once expanded.
        let s = Conversion::new(CameraLog::SLog3Cine);
        let v = (420.0 - 64.0) / 876.0;
        let out = s.apply([v; 3]);
        assert!(out.iter().all(|c| (c - grey).abs() < 1e-6), "{out:?} not {grey}");
        // A neutral stays neutral through the gamut matrix, whatever the camera.
        for log in CameraLog::ALL {
            let out = Conversion::new(log).apply([0.6; 3]);
            assert!((out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6, "{log:?}: {out:?}");
        }
        // 90% white sits under white, and the top of the signal reaches it.
        let white = s.apply([(598.0 - 64.0) / 876.0; 3])[0];
        assert!(white > grey && white < 1.0, "{white}");
        assert!((s.apply([1.0; 3])[0] - 1.0).abs() < 1e-3);
    }

    /// The editor offers each by name, in its own table: a log added here and not there could
    /// only be typed.
    #[test]
    fn the_editor_offers_every_log() {
        let inspector = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui/src/panels/inspector.ts")).unwrap();
        for log in CameraLog::ALL {
            let name = serde_json::to_string(&log).unwrap();
            assert!(inspector.contains(&format!("{name}:")), "{name} is missing from CAMERA_LOGS");
        }
    }

    /// The values footage has always converted to. If one has to change, the change
    /// re-grades every video shot in that log.
    #[test]
    fn each_log_keeps_the_values_it_has_always_given() {
        let c = [0.55, 0.42, 0.33];
        // Worked out apart from this code, from FilmCraft's formulas.
        let want: [(CameraLog, [f64; 3]); 3] = [
            (CameraLog::SLog3Cine, [0.788968430923, 0.40427914021, 0.300270827988]),
            (CameraLog::VLog, [0.793945843013, 0.371598961737, 0.301356151667]),
            (CameraLog::AppleLog, [0.677229665181, 0.323861309216, 0.261714563666]),
        ];
        for (log, w) in want {
            let got = Conversion::new(log).apply(c);
            assert!((0..3).all(|i| (got[i] - w[i]).abs() < 1e-8), "{log:?}: {got:?} not {w:?}");
        }
    }
}
