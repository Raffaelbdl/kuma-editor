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
//! 1. the camera log into Rec. 709 (`log`, [`crate::camera`]), or the input LUT;
//! 2. white balance and exposure, on the light the values stand for (BT.1886's 2.4 gamma
//!    decodes, and encodes back after);
//! 3. lift, gain, then gamma, per channel;
//! 4. contrast about mid grey (18% light, encoded);
//! 5. blacks, shadows, highlights and whites, on luma, every channel moved alike;
//! 6. the curve, on each channel; then red's, green's and blue's own;
//! 7. the hue and saturation curves, on Rec. 709 chroma: hue against saturation, hue
//!    against hue, hue against luma, luma against saturation, saturation against saturation;
//! 8. saturation about Rec. 709 luma, then vibrance (more where there is less);
//! 9. the look, mixed in by `look_amount`;
//! 10. clamped to 0..1, which is all a LUT can hold.
//!
//! Steps 5, 7 and 8's vibrance came after the others, between them: each is skipped while it
//! is neutral, so a grade without them comes out as it always did.

use std::fmt::Write;
use std::sync::Arc;

use crate::model::Grade;

/// The edge of the grid a grade is written as: 33³ is what grading tools exchange, and
/// tetrahedral interpolation between its points is far below 8-bit rounding.
pub const GRID: usize = 33;

/// Display encoding, as BT.1886 has it for a studio monitor.
pub(crate) const DISPLAY_GAMMA: f64 = 2.4;

/// Rec. 709 luma weights.
pub(crate) const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// Rec. 709's chroma scales: Cb is (B' − Y') / CB, Cr is (R' − Y') / CR, each ±0.5 at most.
pub(crate) const CB: f64 = 1.8556;
pub(crate) const CR: f64 = 1.5748;

/// Mid grey: 18% of white's light.
pub(crate) const MID_GREY: f64 = 0.18;

/// Rec. 709 luma of a display-encoded colour (or of light, weighted the same).
pub(crate) fn luma(c: [f64; 3]) -> f64 {
    (0..3).map(|i| LUMA[i] * c[i]).sum()
}

/// The light a display value stands for (BT.1886).
pub(crate) fn decode(v: f64) -> f64 {
    v.max(0.0).powf(DISPLAY_GAMMA)
}

/// The display value that stands for `light`, [`decode`]'s inverse.
pub(crate) fn encode(light: f64) -> f64 {
    light.powf(1.0 / DISPLAY_GAMMA)
}

/// The ranges a grade's settings take: the validator holds a grade to them, and a match
/// solves within them, so a grade it answers can always be written.
impl Grade {
    pub const TEMPERATURE: std::ops::RangeInclusive<f64> = -2.0..=2.0;
    pub const TINT: std::ops::RangeInclusive<f64> = -1.0..=1.0;
    pub const EXPOSURE: std::ops::RangeInclusive<f64> = -5.0..=5.0;
    pub const CONTRAST: std::ops::RangeInclusive<f64> = 0.0..=3.0;
    pub const SATURATION: std::ops::RangeInclusive<f64> = 0.0..=3.0;
    pub const LOOK_AMOUNT: std::ops::RangeInclusive<f64> = 0.0..=1.0;
    /// Each channel of lift, gamma and gain.
    pub const WHEEL: std::ops::RangeInclusive<f64> = -1.0..=1.0;
    /// Blacks, shadows, highlights, whites and vibrance; and what a hue, luma or saturation
    /// curve's points may ask for.
    pub const TONE: std::ops::RangeInclusive<f64> = -1.0..=1.0;
    /// The light: what a grade's keys may set, in [`Grade::light_at`]'s order.
    pub const LIGHT: [&'static str; 3] = ["temperature", "tint", "exposure"];

    /// The light as key `k` has it, the static values filling in what it leaves out; the
    /// static light with no key.
    pub fn light_at(&self, k: Option<&crate::model::Keyframe>) -> [f64; 3] {
        let stat = [self.temperature, self.tint, self.exposure];
        std::array::from_fn(|i| k.map_or(stat[i], |k| k.or(Grade::LIGHT[i], stat[i])))
    }

    /// Whether it leaves every colour as it is — nothing set, which the compiler leaves
    /// out. Read off the settings alone: a grade with a LUT is never neutral, so this needs
    /// no file.
    pub fn is_neutral(&self) -> bool {
        self.input_lut.is_none()
            && self.log.is_none()
            && (self.look.is_none() || self.look_amount == 0.0)
            && std::iter::once(None).chain(self.keyframes.iter().map(Some)).all(|k| {
                let [t, m, e] = self.light_at(k);
                light_gains(t, m, e) == [1.0; 3]
            })
            && self.lift.is_zero()
            && self.gamma.is_zero()
            && self.gain.is_zero()
            && self.contrast == 1.0
            && self.saturation == 1.0
            && [self.blacks, self.shadows, self.highlights, self.whites, self.vibrance] == [0.0; 5]
            && Grading::curves(self).iter().all(Option::is_none)
    }

    /// A keyed grade as the compiler applies it: its input conversion, when it has one, and
    /// the rest without its light — each a grade of its own, a LUT apiece — for the light to
    /// move between them as a gain per channel ([`light_scale`]). Step 2 is a gain on light,
    /// which is a gain on display values too, so this is the grade's own maths; the parts
    /// round and clip to the frame's range between them, as one LUT does not.
    pub fn keyed_parts(&self) -> (Option<Grade>, Grade) {
        let input = (self.log.is_some() || self.input_lut.is_some()).then(|| Grade { log: self.log.clone(), input_lut: self.input_lut.clone(), ..Grade::default() });
        let rest = Grade { log: None, input_lut: None, temperature: 0.0, tint: 0.0, exposure: 0.0, mask: None, keyframes: Vec::new(), ..self.clone() };
        (input, rest)
    }

    /// The tone curves of red, green and blue, as stored.
    pub fn channel_curves(&self) -> [(&'static str, &Vec<[f64; 2]>); 3] {
        [("curve_r", &self.curve_r), ("curve_g", &self.curve_g), ("curve_b", &self.curve_b)]
    }

    /// The curves against hue (whose points wrap round) and against luma and saturation, as
    /// stored, in step 7's order.
    pub fn hue_curves(&self) -> [(&'static str, &Vec<[f64; 2]>, bool); 5] {
        [
            ("hue_sat", &self.hue_sat, true),
            ("hue_hue", &self.hue_hue, true),
            ("hue_luma", &self.hue_luma, true),
            ("luma_sat", &self.luma_sat, false),
            ("sat_sat", &self.sat_sat, false),
        ]
    }
}

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
    /// Read and parse a `.cube` file, its bytes alongside (what the compiler hashes): the
    /// one way every reader of a grade's LUTs reads one, so they agree on what reads.
    pub fn load(path: &std::path::Path) -> Result<(Vec<u8>, Lut), String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let lut = Lut::from_bytes(&bytes)?;
        Ok((bytes, lut))
    }

    /// The LUT at `path`, parsed once and remembered as long as its size and time do not
    /// change — or why it will not read. Every edit validates every grade on every clip, and
    /// the live preview asks for a grade's table at the rate a control is dragged; a 65³
    /// `.cube` takes tens of milliseconds to parse.
    pub fn cached(path: &std::path::Path) -> Result<Arc<Lut>, String> {
        remembered(path, |p| Lut::load(p).map(|(_, lut)| Arc::new(lut)))
    }

    /// Parse a `.cube` file's bytes, which are text (stray non-UTF-8 bytes are in comments
    /// or titles at worst, and do not stop the table reading).
    pub fn from_bytes(bytes: &[u8]) -> Result<Lut, String> {
        Lut::parse(&String::from_utf8_lossy(bytes))
    }

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

/// What `load` makes of the file at `path`, remembered as long as the file's size and time
/// do not change — failures too, so a broken file is not read again on every edit. One store
/// for every kind of file a grade reads, each kind by its own type.
pub(crate) fn remembered<T: Clone + Send + 'static>(path: &std::path::Path, load: impl FnOnce(&std::path::Path) -> Result<T, String>) -> Result<T, String> {
    type Stamp = (u64, Option<std::time::SystemTime>);
    type Seen = std::collections::HashMap<(std::any::TypeId, std::path::PathBuf), (Stamp, Box<dyn std::any::Any + Send>)>;
    static SEEN: std::sync::OnceLock<std::sync::Mutex<Seen>> = std::sync::OnceLock::new();
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let stamp = (meta.len(), meta.modified().ok());
    let key = (std::any::TypeId::of::<T>(), path.to_path_buf());
    let lock = || SEEN.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, read)) = lock().get(&key) {
        if let Some(read) = read.downcast_ref::<Result<T, String>>().filter(|_| *at == stamp) {
            return read.clone();
        }
    }
    let read = load(path);
    lock().insert(key, (stamp, Box::new(read.clone())));
    read
}

/// The files a grade reads, read: its camera log's curve, its input LUT (or the LUT its log
/// is), and its look.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub log: Option<Arc<crate::camera::Conversion>>,
    pub input: Option<Arc<Lut>>,
    pub look: Option<Arc<Lut>>,
}

impl Inputs {
    /// `grade`'s files: its LUTs under `root`, its log found in `log_dirs` (the project's own
    /// `logs/` last), each remembered while it does not change.
    pub fn load(grade: &Grade, root: &std::path::Path, log_dirs: &[std::path::PathBuf]) -> Result<Inputs, String> {
        let lut = |path: &str| Lut::cached(&root.join(path)).map_err(|e| format!("LUT `{path}`: {e}"));
        let mut inputs = Inputs { input: grade.input_lut.as_deref().map(lut).transpose()?, look: grade.look.as_deref().map(lut).transpose()?, log: None };
        if let Some(id) = &grade.log {
            let path = crate::camera::find(id, log_dirs).ok_or_else(|| format!("no camera log `{id}`: add `logs/{id}.json` to the project or a library"))?;
            match crate::camera::load(&path).map_err(|e| format!("camera log `{id}`: {e}"))? {
                crate::camera::Log::Curve(c) => inputs.log = Some(c),
                crate::camera::Log::Lut(cube) => inputs.input = Some(Lut::cached(&cube).map_err(|e| format!("camera log `{id}`'s LUT: {e}"))?),
            }
        }
        Ok(inputs)
    }
}

/// A grade ready to apply: its numbers, its LUTs parsed, and its curve's tangents.
#[derive(Debug, Clone)]
pub struct Grading<'a> {
    grade: &'a Grade,
    inputs: Inputs,
    curve: Option<Curve>,
    /// Red's, green's and blue's own curves.
    channels: [Option<Curve>; 3],
    /// Hue against saturation, hue, luma; luma against saturation; saturation against
    /// saturation — the step 7 curves, in that order.
    hue: [Option<Curve>; 5],
    /// White balance and exposure as one gain per channel on linear light.
    light: [f64; 3],
}

impl<'a> Grading<'a> {
    /// `inputs` are the grade's files, read ([`Inputs::load`]); the caller reads them, since
    /// where a path resolves is the project's business.
    pub fn new(grade: &'a Grade, inputs: Inputs) -> Self {
        let [curve, r, g, b, hue @ ..] = Grading::curves(grade);
        Grading { grade, inputs, curve, channels: [r, g, b], hue, light: light_gains(grade.temperature, grade.tint, grade.exposure) }
    }

    /// Every curve of `grade`, `None` for one that changes nothing: the master curve, red's,
    /// green's, blue's, then step 7's.
    fn curves(grade: &Grade) -> [Option<Curve>; 9] {
        let [r, g, b] = grade.channel_curves().map(|(_, pts)| Curve::through(pts));
        let [hs, hh, hl, ls, ss] = grade.hue_curves().map(|(_, pts, wraps)| if wraps { Curve::periodic(pts) } else { Curve::offset(pts) });
        [Curve::through(&grade.curve), r, g, b, hs, hh, hl, ls, ss]
    }

    /// The same grade with other white balance and exposure: what a match tries in turn,
    /// without parsing the grade's LUTs again for each try.
    pub(crate) fn set_light(&mut self, temperature: f64, tint: f64, exposure: f64) {
        self.light = light_gains(temperature, tint, exposure);
    }

    /// What the grade makes of the display-encoded colour `c`; see the module's steps.
    pub fn apply(&self, c: [f64; 3]) -> [f64; 3] {
        self.after_input(self.input(c))
    }

    /// Step 1 alone: the camera log or the input LUT. It comes before the light, so a match
    /// converts its pixels once and tries each light on what came out.
    pub(crate) fn input(&self, c: [f64; 3]) -> [f64; 3] {
        match (&self.inputs.log, &self.inputs.input) {
            (Some(log), _) => log.apply(c),
            (None, Some(lut)) => lut.apply(c),
            (None, None) => c,
        }
    }

    /// Steps 2 onwards, on what [`Grading::input`] made of a colour.
    pub(crate) fn after_input(&self, mut c: [f64; 3]) -> [f64; 3] {
        let g = self.grade;
        if self.light != [1.0; 3] {
            c = std::array::from_fn(|i| encode(decode(c[i]) * self.light[i]));
        }
        let (lift, gain, gamma) = (g.lift.channels(), g.gain.channels(), g.gamma.channels());
        c = std::array::from_fn(|i| {
            let v = (c[i] + lift[i] * (1.0 - c[i])) * (1.0 + gain[i]);
            if gamma[i] == 0.0 { v } else { v.max(0.0).powf(2f64.powf(-gamma[i])) }
        });
        if g.contrast != 1.0 {
            let pivot = encode(MID_GREY);
            c = c.map(|v| pivot + (v - pivot) * g.contrast);
        }
        let tones = [g.blacks, g.shadows, g.highlights, g.whites];
        if tones != [0.0; 4] {
            // Four bumps that sum to at most 1: at black, a third of the way up, two thirds
            // of the way up and at white.
            let y = luma(c);
            let (v, w) = (y.clamp(0.0, 1.0), 1.0 - y.clamp(0.0, 1.0));
            let weights = [w.powi(4), 6.75 * v * w * w, 6.75 * v * v * w, v.powi(4)];
            let shift: f64 = 0.25 * (0..4).map(|i| tones[i] * weights[i]).sum::<f64>();
            c = c.map(|x| x + shift);
        }
        if let Some(curve) = &self.curve {
            c = c.map(|v| curve.at(v));
        }
        for (i, curve) in self.channels.iter().enumerate() {
            if let Some(curve) = curve {
                c[i] = curve.at(c[i]);
            }
        }
        if self.hue.iter().any(Option::is_some) {
            c = self.hue_curves(c);
        }
        if g.saturation != 1.0 {
            let y = luma(c);
            c = c.map(|v| y + (v - y) * g.saturation);
        }
        if g.vibrance != 0.0 {
            let y = luma(c);
            let spread = (c.iter().copied().fold(f64::MIN, f64::max) - c.iter().copied().fold(f64::MAX, f64::min)).clamp(0.0, 1.0);
            let k = 1.0 + g.vibrance * (1.0 - spread);
            c = c.map(|v| y + (v - y) * k);
        }
        if let Some(look) = self.inputs.look.as_ref().filter(|_| g.look_amount != 0.0) {
            let l = look.apply(c.map(|v| v.clamp(0.0, 1.0)));
            c = std::array::from_fn(|i| c[i] + (l[i] - c[i]) * g.look_amount);
        }
        c.map(|v| v.clamp(0.0, 1.0))
    }

    /// Step 7: every curve read at the colour as it comes in — its hue, luma and saturation —
    /// then chroma turned, scaled, and luma scaled, all in Rec. 709 Y'CbCr, so a grey (no
    /// chroma, no hue) stays where it is whatever the curves say.
    fn hue_curves(&self, c: [f64; 3]) -> [f64; 3] {
        let y = luma(c);
        let (cb, cr) = ((c[2] - y) / CB, (c[0] - y) / CR);
        let hue = cr.atan2(cb) / std::f64::consts::TAU;
        let sat = (2.0 * cb.hypot(cr)).min(1.0);
        let at = |i: usize, v: f64| self.hue[i].as_ref().map_or(0.0, |curve| curve.at(v));
        let scale = (1.0 + at(0, hue)).max(0.0) * (1.0 + at(3, y.clamp(0.0, 1.0))).max(0.0) * (1.0 + at(4, sat)).max(0.0);
        let (sin, cos) = (at(1, hue) * std::f64::consts::PI).sin_cos();
        let (cb, cr) = ((cb * cos - cr * sin) * scale, (cb * sin + cr * cos) * scale);
        let y = y * 2f64.powf(at(2, hue) * sat);
        let (r, b) = (y + CR * cr, y + CB * cb);
        [r, (y - LUMA[0] * r - LUMA[2] * b) / LUMA[1], b]
    }

    /// The grade as a `.cube` of [`GRID`]³ points, red fastest — the file the compiler
    /// hands MLT. The same grade always writes the same bytes.
    pub fn cube(&self) -> String {
        let n = GRID;
        let mut out = format!("# A kuma grade, sampled from its settings: edit those, not this.\nLUT_3D_SIZE {n}\n");
        out.reserve(n * n * n * 27);
        for [x, y, z] in self.grid(n) {
            let _ = writeln!(out, "{x:.6} {y:.6} {z:.6}");
        }
        out
    }

    /// The grade at `n`³ points, red fastest, as a byte a channel: what the editor's live
    /// preview applies to a frame while a control is dragged — the same maths the render
    /// gets as a `.cube`, at the precision a frame on screen has.
    pub fn table_rgb8(&self, n: usize) -> Vec<u8> {
        self.grid(n).flat_map(|c| c.map(|v| (v * 255.0).round() as u8)).collect()
    }

    /// The grade at every point of an `n`³ grid of colours, red fastest.
    fn grid(&self, n: usize) -> impl Iterator<Item = [f64; 3]> + '_ {
        let step = move |i: usize| i as f64 / (n - 1) as f64;
        (0..n * n * n).map(move |i| self.apply([step(i % n), step(i / n % n), step(i / (n * n))]))
    }
}

/// Step 2 as a gain per channel on display values: what a keyed grade's light compiles to.
/// BT.1886 is a power law, so a gain on light is a gain on the values that stand for it.
pub fn light_scale(temperature: f64, tint: f64, exposure: f64) -> [f64; 3] {
    light_gains(temperature, tint, exposure).map(encode)
}

/// White balance and exposure as gains on linear light. Temperature moves red and blue
/// a stop apart at ±1, tint moves green by half a stop, and the three are then scaled so a
/// white keeps its luminance — white balance is not exposure.
fn light_gains(temperature: f64, tint: f64, exposure: f64) -> [f64; 3] {
    if temperature == 0.0 && tint == 0.0 && exposure == 0.0 {
        return [1.0; 3];
    }
    let wb = [2f64.powf(temperature / 2.0), 2f64.powf(-tint / 2.0), 2f64.powf(-temperature / 2.0)];
    let y = luma(wb);
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
    /// Whether it repeats every 1 along `x`, as a curve against hue does.
    wraps: bool,
}

impl Curve {
    /// The curve through `points` and the corners it is not told otherwise about; `None`
    /// when that is the straight line. The points are the validator's to check.
    fn through(points: &[[f64; 2]]) -> Option<Curve> {
        Curve::cornered(points, 1.0)
    }

    /// A curve of amounts, 0 for none: through `points` and `[0, 0]`, `[1, 0]` unless they
    /// say otherwise; `None` when it is 0 everywhere.
    fn offset(points: &[[f64; 2]]) -> Option<Curve> {
        Curve::cornered(points, 0.0)
    }

    /// A curve of amounts round a circle: through `points` (`x` in 0..1) and on round, so
    /// past the last point it bends towards the first; `None` when it is 0 everywhere.
    fn periodic(points: &[[f64; 2]]) -> Option<Curve> {
        if points.iter().all(|p| p[1] == 0.0) {
            return None;
        }
        let pts: Vec<[f64; 2]> = [-1.0, 0.0, 1.0].iter().flat_map(|turn| points.iter().map(move |&[x, y]| [x + turn, y])).collect();
        Some(Curve::monotone(&pts, true))
    }

    /// The curve through `points`, `[0, 0]` before and `[1, end]` after unless points sit
    /// there; `None` when there are none, or when it is the line from one corner to the
    /// other, which changes nothing.
    fn cornered(points: &[[f64; 2]], end: f64) -> Option<Curve> {
        let mut pts = points.to_vec();
        if pts.first()?[0] > 0.0 {
            pts.insert(0, [0.0, 0.0]);
        }
        if pts[pts.len() - 1][0] < 1.0 {
            pts.push([1.0, end]);
        }
        let flat = |[x, y]: [f64; 2]| y == x * end;
        (!pts.iter().all(|&p| flat(p))).then(|| Curve::monotone(&pts, false))
    }

    fn monotone(pts: &[[f64; 2]], wraps: bool) -> Curve {
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
        Curve { x, y, m, wraps }
    }

    /// The curve at `v`; past its ends it goes on straight, along its end slopes (or round,
    /// for one that wraps).
    fn at(&self, v: f64) -> f64 {
        let v = if self.wraps { v.rem_euclid(1.0) } else { v };
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
        Grading::new(g, Inputs::default()).apply(c)
    }

    #[test]
    fn a_default_grade_is_neutral_and_changes_nothing() {
        let g = Grade::default();
        let grading = Grading::new(&g, Inputs::default());
        assert!(g.is_neutral());
        for c in [[0.0, 0.0, 0.0], [0.25, 0.5, 0.75], [1.0, 1.0, 1.0]] {
            assert!(close(grading.apply(c), c));
        }
        // A curve along the diagonal is no curve.
        assert!(Grade { curve: vec![[0.5, 0.5]], ..Grade::default() }.is_neutral());
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

    /// The same, for the steps that came later: tones, vibrance, the channels' curves and
    /// step 7's. Worked out apart from this code, from the formulas in the module's steps.
    #[test]
    fn the_later_steps_keep_the_values_they_have_always_given() {
        let c = [0.6, 0.5, 0.4];
        let offset = vec![[0.3, -0.5], [0.6, 0.4]];
        let hues = vec![[0.1, 0.5], [0.4, -0.25], [0.8, 0.2]];
        let cases: Vec<(Grade, [f64; 3], [f64; 3])> = vec![
            (Grade { shadows: 1.0, ..Grade::default() }, [0.25; 3], [0.4873046875; 3]),
            (Grade { highlights: 1.0, ..Grade::default() }, [0.6; 3], [0.843; 3]),
            (Grade { whites: -1.0, ..Grade::default() }, [0.9; 3], [0.735975; 3]),
            (Grade { blacks: 0.5, ..Grade::default() }, [0.0; 3], [0.125; 3]),
            (Grade { vibrance: 1.0, ..Grade::default() }, c, [0.668768, 0.488768, 0.308768]),
            (Grade { curve_r: vec![[0.5, 0.6]], ..Grade::default() }, [0.5; 3], [0.6, 0.5, 0.5]),
            (Grade { curve_b: vec![[0.5, 0.25]], ..Grade::default() }, [0.5; 3], [0.5, 0.5, 0.25]),
            (Grade { hue_hue: vec![[0.0, 1.0]], ..Grade::default() }, c, [0.42808, 0.52808, 0.62808]),
            (Grade { hue_hue: vec![[0.0, 0.25]], ..Grade::default() }, c, [0.506387114, 0.531685604, 0.361780545]),
            (Grade { hue_sat: vec![[0.3, -1.0]], ..Grade::default() }, [1.0, 0.0, 0.0], [0.2126; 3]),
            (Grade { hue_sat: hues.clone(), ..Grade::default() }, c, [0.579013908, 0.503427696, 0.427841484]),
            (Grade { hue_luma: vec![[0.0, 1.0]], ..Grade::default() }, [1.0, 0.0, 0.0], [1.0, 0.2126, 0.2126]),
            (Grade { hue_luma: vec![[0.0, -1.0]], ..Grade::default() }, c, [0.544639051, 0.444639051, 0.344639051]),
            (Grade { luma_sat: offset.clone(), ..Grade::default() }, c, [0.618968874, 0.49690178, 0.374834685]),
            (Grade { sat_sat: offset, ..Grade::default() }, c, [0.570613718, 0.504799714, 0.43898571]),
        ];
        for (g, c, want) in cases {
            assert!(!g.is_neutral(), "{g:?}");
            let got = graded(&g, c);
            assert!((0..3).all(|i| (got[i] - want[i]).abs() < 1e-8), "{g:?} on {c:?}: {got:?}, want {want:?}");
        }
    }

    #[test]
    fn a_grey_keeps_its_place_whatever_the_hue_curves_say() {
        let g = Grade {
            hue_sat: vec![[0.2, 1.0]],
            hue_hue: vec![[0.5, 0.7]],
            hue_luma: vec![[0.7, -1.0]],
            luma_sat: vec![[0.5, 1.0]],
            sat_sat: vec![[0.0, 1.0]],
            vibrance: 1.0,
            ..Grade::default()
        };
        for v in [0.0, 0.18, 0.5, 1.0] {
            assert!(close(graded(&g, [v; 3]), [v; 3]), "{v}");
        }
        // Curves at nought are no curves.
        let flat = Grade { hue_sat: vec![[0.2, 0.0]], luma_sat: vec![[0.5, 0.0]], curve_g: vec![[0.3, 0.3]], ..Grade::default() };
        assert!(flat.is_neutral());
    }

    /// `g` on `c`, its camera log read from the starter set.
    fn logged(g: &Grade, c: [f64; 3]) -> [f64; 3] {
        let starter = [std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../library/logs"))];
        Grading::new(g, Inputs::load(g, std::path::Path::new("."), &starter).unwrap()).apply(c)
    }

    #[test]
    fn a_camera_log_comes_first_and_is_never_neutral() {
        let log = |id: &str| Some(id.to_string());
        let g = Grade { log: log("s-log3-cine"), saturation: 0.0, ..Grade::default() };
        assert!(!Grade { log: log("v-log"), ..Grade::default() }.is_neutral());
        // The conversion, then the rest of the grade on what it gave.
        let c = [0.55, 0.42, 0.33];
        let converted = logged(&Grade { log: log("s-log3-cine"), ..Grade::default() }, c);
        let y = luma(converted);
        assert!(close(logged(&g, c), [y; 3]), "{:?}", logged(&g, c));
    }

    /// A keyed grade's parts — input, a gain, the rest — make what the grade makes, for a
    /// colour none of them clips.
    #[test]
    fn a_keyed_grade_in_parts_is_the_grade() {
        let g = Grade { log: Some("v-log".into()), temperature: 0.4, tint: -0.2, exposure: 0.3, lift: Rgb { r: 0.02, g: 0.0, b: -0.02 }, contrast: 1.2, saturation: 0.9, ..Grade::default() };
        let (input, rest) = g.keyed_parts();
        let input = input.unwrap();
        assert!(rest.log.is_none() && rest.temperature == 0.0 && rest.contrast == 1.2);
        let scale = light_scale(g.temperature, g.tint, g.exposure);
        for c in [[0.4, 0.42, 0.38], [0.3, 0.35, 0.45]] {
            let mid = logged(&input, c);
            let parts = graded(&rest, std::array::from_fn(|i| mid[i] * scale[i]));
            assert!(close(parts, logged(&g, c)), "{c:?}: {parts:?} vs {:?}", logged(&g, c));
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

    /// The curves of amounts, which the editor draws with its copy too
    /// (`ui/test/curve.test.mjs` pins the same values).
    #[test]
    fn the_amount_curves_draw_what_the_editor_draws() {
        let xs = [0.0, 0.1, 0.2, 0.3, 0.35, 0.5, 0.52, 0.6, 0.7, 0.8, 0.9, 1.0];
        let offset = Curve::offset(&[[0.3, -0.5], [0.6, 0.4]]).unwrap();
        let want = [0.0, -0.203703704, -0.407407407, -0.5, -0.433333333, 0.166666667, 0.242133333, 0.4, 0.35625, 0.25, 0.11875, 0.0];
        let periodic = Curve::periodic(&[[0.1, 0.5], [0.4, -0.25], [0.8, 0.2]]).unwrap();
        let round = [0.445833333, 0.5, 0.305555556, -0.055555556, -0.194444444, -0.199609375, -0.179575, -0.078125, 0.069921875, 0.2, 0.325, 0.445833333];
        for (i, x) in xs.iter().enumerate() {
            assert!((offset.at(*x) - want[i]).abs() < 1e-8, "offset at {x}: {}", offset.at(*x));
            assert!((periodic.at(*x) - round[i]).abs() < 1e-8, "periodic at {x}: {}", periodic.at(*x));
        }
        // Round the circle and on.
        assert!((periodic.at(1.1) - periodic.at(0.1)).abs() < 1e-12 && (periodic.at(-0.2) - periodic.at(0.8)).abs() < 1e-12);
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
        let g = Grade { saturation: 0.5, gain: Rgb { r: 0.1, g: 0.0, b: -0.1 }, ..Grade::default() };
        let lut_text = Grading::new(&g, Inputs::default()).cube();
        assert!(lut_text.contains("LUT_3D_SIZE 33\n"));
        let back = Lut::parse(&lut_text).unwrap();
        // On a grid point the file says exactly what the grade does, to its six places.
        let p = [4.0 / 32.0, 16.0 / 32.0, 30.0 / 32.0];
        assert!((0..3).all(|i| (back.apply(p)[i] - graded(&g, p)[i]).abs() < 1e-6));
    }

    #[test]
    fn the_luts_go_either_side_of_the_grade() {
        let invert = Arc::new(Lut::parse("LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap());
        // Input first: inverted black is white, which a gain cannot raise past 1.
        let g = Grade { gain: Rgb { r: -0.5, g: -0.5, b: -0.5 }, ..Grade::default() };
        assert!(close(Grading::new(&g, Inputs { input: Some(invert.clone()), ..Inputs::default() }).apply([0.0; 3]), [0.5; 3]));
        // The look last, and by its amount: half of an inverted grey is that grey.
        let g = Grade { look_amount: 0.5, ..Grade::default() };
        assert!(close(Grading::new(&g, Inputs { look: Some(invert), ..Inputs::default() }).apply([0.2; 3]), [0.5; 3]));
        // Whether a grade with a look changes anything is in its settings, not its file.
        assert!(Grade { look: Some("off.cube".into()), look_amount: 0.0, ..Grade::default() }.is_neutral());
        assert!(!Grade { look: Some("on.cube".into()), ..Grade::default() }.is_neutral());
    }
}
