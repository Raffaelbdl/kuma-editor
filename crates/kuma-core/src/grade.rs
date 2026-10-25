//! What a [`Grade`] does to a colour, and the `.cube` LUTs it reads and becomes.
//!
//! The compiler never asks MLT to grade step by step: it samples [`Grading::apply`] on a
//! grid, writes the grid as one 3D LUT ([`Grading::cube`]) and applies that, so a grade is
//! one filter whatever it holds and the picture is rounded once. Which is also why these
//! maths are **append-only in behaviour**, like `_vendor/`: a finished video's grade is
//! recomputed whenever it compiles, so a step that starts returning something else
//! re-grades it. Add a field rather than change what one means; the tests pin values.
//!
//! The steps, in order, on display-encoded R'G'B' from 0 to 1:
//!
//! 1. the input LUT;
//! 2. white balance and exposure, on the light the values stand for (BT.1886's 2.4 gamma
//!    decodes, and encodes back after);
//! 3. lift, gain, then gamma, per channel;
//! 4. contrast about mid grey (18% light, encoded);
//! 5. the curve, on each channel;
//! 6. saturation about Rec. 709 luma;
//! 7. the look, mixed in by `look_amount`;
//! 8. clamped to 0..1, which is all a LUT can hold.

use crate::model::Grade;

/// The edge of the grid a grade is written as: 33³ is what grading tools exchange, and
/// tetrahedral interpolation between its points is far below 8-bit rounding.
pub const GRID: usize = 33;

/// Display encoding, as BT.1886 has it for a studio monitor.
pub(crate) const DISPLAY_GAMMA: f64 = 2.4;

/// Rec. 709 luma weights.
pub(crate) const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// A parsed `.cube`: Adobe's format, which Resolve, ffmpeg and the cameras' vendors write.
#[derive(Debug, Clone, PartialEq)]
pub struct Lut {
    /// Input values map from `min..max` per channel onto the table.
    min: [f64; 3],
    max: [f64; 3],
    table: Table,
}

#[derive(Debug, Clone, PartialEq)]
enum Table {
    /// A curve per channel, `size` entries.
    D1(Vec<[f64; 3]>),
    /// `size`³ entries, red fastest.
    D3 { size: usize, data: Vec<[f64; 3]> },
}

impl Lut {
    /// Parse a `.cube`'s text. Keywords it does not know are skipped, as the format says.
    pub fn parse(text: &str) -> Result<Lut, String> {
        let (mut size1, mut size3) = (None, None);
        let (mut min, mut max) = ([0.0; 3], [1.0; 3]);
        let mut data = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = |what: &str| format!("line {}: {what}", n + 1);
            let mut words = line.split_whitespace();
            let first = words.next().unwrap_or_default();
            if first.parse::<f64>().is_ok() {
                let v: Vec<f64> = line.split_whitespace().map(str::parse).collect::<Result<_, _>>().map_err(|_| at("not three numbers"))?;
                match v[..] {
                    [r, g, b] if v.iter().all(|x| x.is_finite()) => data.push([r, g, b]),
                    _ => return Err(at("not three numbers")),
                }
                continue;
            }
            if !data.is_empty() {
                return Err(at(&format!("`{first}` after the table started")));
            }
            let nums = |words: std::str::SplitWhitespace| -> Result<Vec<f64>, String> { words.map(|w| w.parse::<f64>().map_err(|_| at(&format!("`{first}` takes numbers")))).collect() };
            match first {
                "LUT_1D_SIZE" | "LUT_3D_SIZE" => {
                    let s = match nums(words)?[..] {
                        [s] if s.fract() == 0.0 && s >= 2.0 => s as usize,
                        _ => return Err(at(&format!("`{first}` takes one whole number, 2 or more"))),
                    };
                    let (slot, most) = if first == "LUT_1D_SIZE" { (&mut size1, 65536) } else { (&mut size3, 256) };
                    if s > most {
                        return Err(at(&format!("`{first}` is at most {most}")));
                    }
                    *slot = Some(s);
                }
                "DOMAIN_MIN" | "DOMAIN_MAX" => match nums(words)?[..] {
                    [r, g, b] => *(if first == "DOMAIN_MIN" { &mut min } else { &mut max }) = [r, g, b],
                    _ => return Err(at(&format!("`{first}` takes three numbers"))),
                },
                // Resolve's spelling of one domain for all three channels.
                "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => match nums(words)?[..] {
                    [lo, hi] => (min, max) = ([lo; 3], [hi; 3]),
                    _ => return Err(at(&format!("`{first}` takes two numbers"))),
                },
                _ => {}
            }
        }
        if (0..3).any(|c| !min[c].is_finite() || !max[c].is_finite() || max[c] <= min[c]) {
            return Err("the domain's maximum must be above its minimum".into());
        }
        let table = match (size1, size3) {
            (Some(_), Some(_)) => return Err("both LUT_1D_SIZE and LUT_3D_SIZE: kuma reads one table per file".into()),
            (None, None) => return Err("no LUT_1D_SIZE or LUT_3D_SIZE".into()),
            (Some(s), None) if data.len() == s => Table::D1(data),
            (None, Some(s)) if data.len() == s * s * s => Table::D3 { size: s, data },
            (Some(s), None) => return Err(format!("LUT_1D_SIZE {s} needs {s} rows, and there are {}", data.len())),
            (None, Some(s)) => return Err(format!("LUT_3D_SIZE {s} needs {} rows, and there are {}", s * s * s, data.len())),
        };
        Ok(Lut { min, max, table })
    }

    /// The colour `c` becomes: linear between a 1D table's entries, tetrahedral inside a
    /// 3D table's cells — what ffmpeg's `lut3d` does by default, and what keeps a grey grey.
    pub fn apply(&self, c: [f64; 3]) -> [f64; 3] {
        let at = |i: usize, n: usize| ((c[i] - self.min[i]) / (self.max[i] - self.min[i])).clamp(0.0, 1.0) * (n - 1) as f64;
        match &self.table {
            Table::D1(rows) => {
                let n = rows.len();
                std::array::from_fn(|i| {
                    let x = at(i, n);
                    let lo = (x.floor() as usize).min(n - 2);
                    let f = x - lo as f64;
                    rows[lo][i] + (rows[lo + 1][i] - rows[lo][i]) * f
                })
            }
            Table::D3 { size: n, data } => {
                let n = *n;
                let (x, y, z) = (at(0, n), at(1, n), at(2, n));
                let (r0, g0, b0) = ((x.floor() as usize).min(n - 2), (y.floor() as usize).min(n - 2), (z.floor() as usize).min(n - 2));
                let (dr, dg, db) = (x - r0 as f64, y - g0 as f64, z - b0 as f64);
                let p = |r: usize, g: usize, b: usize| data[(r0 + r) + (g0 + g) * n + (b0 + b) * n * n];
                let (c000, c111) = (p(0, 0, 0), p(1, 1, 1));
                // The cube's diagonal splits it into six tetrahedra; the ordering of the
                // three fractions says which one the point is in, and its three edges.
                let (a, wa, b, wb, w3) = if dr > dg {
                    if dg > db {
                        (p(1, 0, 0), dr, p(1, 1, 0), dg, db)
                    } else if dr > db {
                        (p(1, 0, 0), dr, p(1, 0, 1), db, dg)
                    } else {
                        (p(0, 0, 1), db, p(1, 0, 1), dr, dg)
                    }
                } else if db > dg {
                    (p(0, 0, 1), db, p(0, 1, 1), dg, dr)
                } else if db > dr {
                    (p(0, 1, 0), dg, p(0, 1, 1), db, dr)
                } else {
                    (p(0, 1, 0), dg, p(1, 1, 0), dr, db)
                };
                std::array::from_fn(|i| c000[i] + wa * (a[i] - c000[i]) + wb * (b[i] - a[i]) + w3 * (c111[i] - b[i]))
            }
        }
    }
}

/// A grade ready to apply: its numbers, its LUTs parsed, and its curve's tangents.
#[derive(Debug, Clone)]
pub struct Grading<'a> {
    grade: &'a Grade,
    input: Option<Lut>,
    look: Option<Lut>,
    curve: Option<Curve>,
    /// White balance and exposure as one gain per channel on linear light.
    light: [f64; 3],
}

impl<'a> Grading<'a> {
    /// `input` and `look` are the grade's LUT files, parsed; the caller reads them, since
    /// where a path resolves is the project's business.
    pub fn new(grade: &'a Grade, input: Option<Lut>, look: Option<Lut>) -> Self {
        let curve = Curve::through(&grade.curve);
        Grading { grade, input, look, curve, light: light_gains(grade.temperature, grade.tint, grade.exposure) }
    }

    /// The same grade with other white balance and exposure: what a match tries in turn,
    /// without parsing the grade's LUTs again for each try.
    pub(crate) fn set_light(&mut self, temperature: f64, tint: f64, exposure: f64) {
        self.light = light_gains(temperature, tint, exposure);
    }

    /// Whether it leaves every colour as it is — a grade with nothing set, which the
    /// compiler then leaves out.
    pub fn is_neutral(&self) -> bool {
        let g = self.grade;
        self.input.is_none() && (self.look.is_none() || g.look_amount == 0.0) && self.curve.is_none() && self.light == [1.0; 3]
            && g.lift.is_zero() && g.gamma.is_zero() && g.gain.is_zero() && g.contrast == 1.0 && g.saturation == 1.0
    }

    /// What the grade makes of the display-encoded colour `c`; see the module's steps.
    pub fn apply(&self, c: [f64; 3]) -> [f64; 3] {
        let g = self.grade;
        let mut c = match &self.input {
            Some(lut) => lut.apply(c),
            None => c,
        };
        if self.light != [1.0; 3] {
            c = std::array::from_fn(|i| (c[i].max(0.0).powf(DISPLAY_GAMMA) * self.light[i]).powf(1.0 / DISPLAY_GAMMA));
        }
        let (lift, gain, gamma) = (g.lift.channels(), g.gain.channels(), g.gamma.channels());
        c = std::array::from_fn(|i| {
            let v = (c[i] + lift[i] * (1.0 - c[i])) * (1.0 + gain[i]);
            if gamma[i] == 0.0 { v } else { v.max(0.0).powf(2f64.powf(-gamma[i])) }
        });
        if g.contrast != 1.0 {
            let pivot = 0.18f64.powf(1.0 / DISPLAY_GAMMA);
            c = c.map(|v| pivot + (v - pivot) * g.contrast);
        }
        if let Some(curve) = &self.curve {
            c = c.map(|v| curve.at(v));
        }
        if g.saturation != 1.0 {
            let y: f64 = (0..3).map(|i| LUMA[i] * c[i]).sum();
            c = c.map(|v| y + (v - y) * g.saturation);
        }
        if let Some(look) = self.look.as_ref().filter(|_| g.look_amount != 0.0) {
            let l = look.apply(c.map(|v| v.clamp(0.0, 1.0)));
            c = std::array::from_fn(|i| c[i] + (l[i] - c[i]) * g.look_amount);
        }
        c.map(|v| v.clamp(0.0, 1.0))
    }

    /// The grade as a `.cube` of [`GRID`]³ points, red fastest — the file the compiler
    /// hands MLT. The same grade always writes the same bytes.
    pub fn cube(&self) -> String {
        let n = GRID;
        let step = |i: usize| i as f64 / (n - 1) as f64;
        let mut out = format!("# A kuma grade, sampled from its settings: edit those, not this.\nLUT_3D_SIZE {n}\n");
        out.reserve(n * n * n * 27);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let [x, y, z] = self.apply([step(r), step(g), step(b)]);
                    out.push_str(&format!("{x:.6} {y:.6} {z:.6}\n"));
                }
            }
        }
        out
    }
}

/// White balance and exposure as gains on linear light. Temperature moves red and blue
/// a stop apart at ±1, tint moves green by half a stop, and the three are then scaled so a
/// white keeps its luminance — white balance is not exposure.
fn light_gains(temperature: f64, tint: f64, exposure: f64) -> [f64; 3] {
    if temperature == 0.0 && tint == 0.0 && exposure == 0.0 {
        return [1.0; 3];
    }
    let wb = [2f64.powf(temperature / 2.0), 2f64.powf(-tint / 2.0), 2f64.powf(-temperature / 2.0)];
    let y: f64 = (0..3).map(|i| LUMA[i] * wb[i]).sum();
    let ev = 2f64.powf(exposure);
    wb.map(|w| w / y * ev)
}

/// A monotone cubic through points (Fritsch–Carlson): between two points it never leaves
/// the range they span, so a curve cannot ring past what its points ask for.
#[derive(Debug, Clone)]
struct Curve {
    x: Vec<f64>,
    y: Vec<f64>,
    m: Vec<f64>,
}

impl Curve {
    /// The curve through `points` and the corners it is not told otherwise about; `None`
    /// when that is the straight line. The points are the validator's to check.
    fn through(points: &[[f64; 2]]) -> Option<Curve> {
        if points.is_empty() {
            return None;
        }
        let mut pts = points.to_vec();
        if pts[0][0] > 0.0 {
            pts.insert(0, [0.0, 0.0]);
        }
        if pts[pts.len() - 1][0] < 1.0 {
            pts.push([1.0, 1.0]);
        }
        if pts.iter().all(|[x, y]| x == y) {
            return None;
        }
        let (x, y): (Vec<f64>, Vec<f64>) = pts.iter().map(|p| (p[0], p[1])).unzip();
        let n = x.len();
        let d: Vec<f64> = (0..n - 1).map(|k| (y[k + 1] - y[k]) / (x[k + 1] - x[k])).collect();
        let mut m = vec![0.0; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for k in 1..n - 1 {
            m[k] = if d[k - 1] * d[k] > 0.0 { (d[k - 1] + d[k]) / 2.0 } else { 0.0 };
        }
        for k in 0..n - 1 {
            if d[k] == 0.0 {
                m[k] = 0.0;
                m[k + 1] = 0.0;
                continue;
            }
            let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
            let h = a * a + b * b;
            if h > 9.0 {
                let t = 3.0 / h.sqrt();
                m[k] = t * a * d[k];
                m[k + 1] = t * b * d[k];
            }
        }
        Some(Curve { x, y, m })
    }

    /// The curve at `v`; past its ends it goes on straight, along its end slopes.
    fn at(&self, v: f64) -> f64 {
        let (x, y, m) = (&self.x, &self.y, &self.m);
        let n = x.len();
        if v <= x[0] {
            return y[0] + (v - x[0]) * m[0];
        }
        if v >= x[n - 1] {
            return y[n - 1] + (v - x[n - 1]) * m[n - 1];
        }
        let k = x.partition_point(|&xi| xi <= v) - 1;
        let h = x[k + 1] - x[k];
        let t = (v - x[k]) / h;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * y[k] + (t3 - 2.0 * t2 + t) * h * m[k] + (-2.0 * t3 + 3.0 * t2) * y[k + 1] + (t3 - t2) * h * m[k + 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgb;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-6)
    }

    fn graded(g: &Grade, c: [f64; 3]) -> [f64; 3] {
        Grading::new(g, None, None).apply(c)
    }

    #[test]
    fn a_default_grade_is_neutral_and_changes_nothing() {
        let g = Grade::default();
        let grading = Grading::new(&g, None, None);
        assert!(grading.is_neutral());
        for c in [[0.0, 0.0, 0.0], [0.25, 0.5, 0.75], [1.0, 1.0, 1.0]] {
            assert!(close(grading.apply(c), c));
        }
        // A curve along the diagonal is no curve.
        let g = Grade { curve: vec![[0.5, 0.5]], ..Grade::default() };
        assert!(Grading::new(&g, None, None).is_neutral());
    }

    /// The values a finished video's grade comes out at. These are the module's contract:
    /// if one of them has to change, the change re-grades every video that uses the step.
    #[test]
    fn each_step_keeps_the_values_it_has_always_given() {
        let grey = [0.5, 0.5, 0.5];
        let cases: Vec<(Grade, [f64; 3], [f64; 3])> = vec![
            (Grade { exposure: 1.0, ..Grade::default() }, grey, [0.66742, 0.66742, 0.66742]),
            (Grade { temperature: 1.0, ..Grade::default() }, grey, [0.56229, 0.48669, 0.42124]),
            (Grade { tint: 1.0, ..Grade::default() }, grey, [0.55145, 0.4773, 0.55145]),
            (Grade { lift: Rgb { r: 0.2, g: 0.0, b: 0.0 }, ..Grade::default() }, [0.0; 3], [0.2, 0.0, 0.0]),
            (Grade { gain: Rgb { r: 0.0, g: 0.0, b: 0.5 }, ..Grade::default() }, grey, [0.5, 0.5, 0.75]),
            (Grade { gamma: Rgb { r: 1.0, g: 1.0, b: 1.0 }, ..Grade::default() }, [0.25; 3], [0.5; 3]),
            (Grade { contrast: 2.0, ..Grade::default() }, grey, [0.51056, 0.51056, 0.51056]),
            (Grade { saturation: 0.0, ..Grade::default() }, [1.0, 0.0, 0.0], [0.2126, 0.2126, 0.2126]),
            (Grade { curve: vec![[0.5, 0.6]], ..Grade::default() }, grey, [0.6, 0.6, 0.6]),
        ];
        for (g, c, want) in cases {
            let got = graded(&g, c);
            assert!((0..3).all(|i| (got[i] - want[i]).abs() < 5e-5), "{g:?} on {c:?}: {got:?}, want {want:?}");
        }
    }

    #[test]
    fn white_balance_keeps_a_whites_luminance_and_exposure_scales_light() {
        let g = Grade { temperature: 0.7, tint: -0.3, ..Grade::default() };
        let gains = light_gains(g.temperature, g.tint, 0.0);
        assert!(((0..3).map(|i| LUMA[i] * gains[i]).sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(gains[0] > 1.0 && gains[2] < 1.0, "warmer is redder and less blue: {gains:?}");
        let lit = light_gains(0.0, 0.0, -2.0);
        assert!(close(lit, [0.25; 3]));
    }

    #[test]
    fn the_curve_passes_through_its_points_and_never_overshoots_them() {
        let pts = vec![[0.2, 0.1], [0.5, 0.5], [0.55, 0.9], [0.8, 0.92]];
        let c = Curve::through(&pts).unwrap();
        for [x, y] in &pts {
            assert!((c.at(*x) - y).abs() < 1e-12);
        }
        // Between two points the curve stays between their values.
        let all: Vec<[f64; 2]> = [[0.0, 0.0]].into_iter().chain(pts.iter().copied()).chain([[1.0, 1.0]]).collect();
        for w in all.windows(2) {
            for s in 0..=50 {
                let x = w[0][0] + (w[1][0] - w[0][0]) * s as f64 / 50.0;
                let (lo, hi) = (w[0][1].min(w[1][1]), w[0][1].max(w[1][1]));
                let v = c.at(x);
                assert!(v >= lo - 1e-12 && v <= hi + 1e-12, "at {x}: {v} outside {lo}..{hi}");
            }
        }
    }

    /// The editor draws the curve with its own copy of this interpolation
    /// (`ui/src/curve.ts`), and its test pins these same values: change one, change both.
    #[test]
    fn the_curve_draws_what_the_editor_draws() {
        let xs = [0.0, 0.1, 0.2, 0.3, 0.35, 0.5, 0.52, 0.6, 0.7, 0.8, 0.9, 1.0];
        let cases: [(&[[f64; 2]], [f64; 12]); 2] = [
            (
                &[[0.0, 0.05], [0.2, 0.1], [0.5, 0.5], [0.55, 0.9], [0.8, 0.92]],
                [0.05, 0.065497641, 0.1, 0.142771436, 0.174453941, 0.5, 0.668117659, 0.909632625, 0.918197499, 0.92, 0.950355808, 1.0],
            ),
            (&[[0.25, 0.22], [0.75, 0.8]], [0.0, 0.08464, 0.17152, 0.27314, 0.32992, 0.5125, 0.53727376, 0.63482, 0.74866, 0.84576, 0.92432, 1.0]),
        ];
        for (points, want) in cases {
            let c = Curve::through(points).unwrap();
            for (x, w) in xs.iter().zip(want) {
                assert!((c.at(*x) - w).abs() < 1e-8, "{points:?} at {x}: {} not {w}", c.at(*x));
            }
        }
    }

    const IDENTITY_2: &str = "TITLE \"identity\"\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";

    #[test]
    fn a_cube_parses_and_an_identity_cube_changes_nothing() {
        let lut = Lut::parse(IDENTITY_2).unwrap();
        for c in [[0.1, 0.7, 0.3], [0.9, 0.2, 0.5], [0.5, 0.5, 0.5]] {
            assert!(close(lut.apply(c), c), "{c:?} → {:?}", lut.apply(c));
        }
        let inverted = Lut::parse("LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
        assert!(close(inverted.apply([0.25, 0.5, 1.0]), [0.75, 0.5, 0.0]));
        // A domain other than 0..1 maps onto the table.
        let wide = Lut::parse("DOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\nLUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
        assert!(close(wide.apply([1.0, 0.5, 2.0]), [0.5, 0.25, 1.0]));
    }

    #[test]
    fn a_broken_cube_says_what_is_wrong() {
        for (text, says) in [
            ("0 0 0\n", "no LUT_1D_SIZE"),
            ("LUT_3D_SIZE 2\n0 0 0\n", "needs 8 rows"),
            ("LUT_3D_SIZE 2\n0 0\n", "line 2"),
            ("LUT_3D_SIZE 1\n0 0 0\n", "2 or more"),
            ("LUT_1D_SIZE 2\nLUT_3D_SIZE 2\n", "both"),
        ] {
            let e = Lut::parse(text).unwrap_err();
            assert!(e.contains(says), "{text:?}: {e}");
        }
    }

    #[test]
    fn the_grid_is_the_grade_sampled() {
        let lut_text = Grading::new(&Grade { saturation: 0.5, gain: Rgb { r: 0.1, g: 0.0, b: -0.1 }, ..Grade::default() }, None, None).cube();
        assert!(lut_text.contains("LUT_3D_SIZE 33\n"));
        let back = Lut::parse(&lut_text).unwrap();
        let g = Grade { saturation: 0.5, gain: Rgb { r: 0.1, g: 0.0, b: -0.1 }, ..Grade::default() };
        // On a grid point the file says exactly what the grade does, to its six places.
        let p = [4.0 / 32.0, 16.0 / 32.0, 30.0 / 32.0];
        assert!((0..3).all(|i| (back.apply(p)[i] - graded(&g, p)[i]).abs() < 1e-6));
    }

    #[test]
    fn the_luts_go_either_side_of_the_grade() {
        let invert = Lut::parse("LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
        // Input first: inverted black is white, which a gain cannot raise past 1.
        let g = Grade { gain: Rgb { r: -0.5, g: -0.5, b: -0.5 }, ..Grade::default() };
        assert!(close(Grading::new(&g, Some(invert.clone()), None).apply([0.0; 3]), [0.5; 3]));
        // The look last, and by its amount: half of an inverted grey is that grey.
        let g = Grade { look_amount: 0.5, ..Grade::default() };
        assert!(close(Grading::new(&g, None, Some(invert.clone())).apply([0.2; 3]), [0.5; 3]));
        let off = Grade { look_amount: 0.0, ..Grade::default() };
        assert!(Grading::new(&off, None, Some(invert)).is_neutral());
    }
}
