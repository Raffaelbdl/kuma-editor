//! What a region of a picture is, in the grade's own terms, and the grade that makes one
//! region look like another.
//!
//! Pixels are display-encoded R'G'B' from 0 to 1, what [`crate::grade`] grades. Two of the
//! grade's steps — white balance and exposure — are gains on light, so how bright and how
//! warm or green a region is can be said in the units those steps take: [`ColorStats`]'s
//! `exposure` is its light in stops from mid grey, and its `balance` the temperature and
//! tint that would turn a grey into its colour. Matching is then a difference of those,
//! refined through the whole grade on the region's own pixels ([`match_light`]).

use serde::Serialize;

use crate::grade::{Grading, Lut, DISPLAY_GAMMA, LUMA};
use crate::model::Grade;

/// Mid grey: 18% of white's light.
const MID_GREY: f64 = 0.18;

/// The least light a channel is taken to have, so a black region has a balance and an
/// exposure (about -17.5 stops) rather than infinities.
const FLOOR: f64 = 1e-6;

/// What a region of a picture is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColorStats {
    pub pixels: usize,
    /// The mean colour as displayed, R'G'B' 0..1.
    pub mean: [f64; 3],
    /// Rec. 709 luma of the displayed values, 0..1 (×100 reads as IRE).
    pub luma: Levels,
    /// The region's light in stops from mid grey (its mean luminance, decoded): 0 is mid
    /// grey, -1 half as much light. Two regions' difference is the `exposure` that matches
    /// them.
    pub exposure: f64,
    /// The region's cast in the grade's own units: the `temperature` and `tint` that make a
    /// grey look like its mean light. Warm is temperature above 0, magenta tint above 0; a
    /// grade of minus these makes the region neutral.
    pub balance: Balance,
    /// The mean of each pixel's largest channel less its smallest: 0 is grey.
    pub saturation: f64,
    /// The share of pixels with a channel at full scale (254/255 or more): highlights
    /// with nothing left in them, which no grade brings back.
    pub clipped: f64,
    /// The share of pixels with every channel at 2/255 or less: shadows crushed to black.
    pub crushed: f64,
}

/// Where a region's luma lies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Levels {
    pub min: f64,
    pub p5: f64,
    pub median: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
}

/// A cast, as the grade's white balance would make it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Balance {
    pub temperature: f64,
    pub tint: f64,
}

impl ColorStats {
    /// The statistics of `pixels`, or `None` for none.
    pub fn of(pixels: &[[f64; 3]]) -> Option<ColorStats> {
        if pixels.is_empty() {
            return None;
        }
        let n = pixels.len() as f64;
        let mut mean = [0.0; 3];
        let mut saturation = 0.0;
        let (mut clipped, mut crushed) = (0usize, 0usize);
        // Luma to the nearest 1/1023 for the percentiles: finer than 8-bit pixels hold.
        const BINS: usize = 1024;
        let mut hist = [0usize; BINS];
        let (mut lo, mut hi, mut luma_sum) = (f64::INFINITY, f64::NEG_INFINITY, 0.0);
        for p in pixels {
            for i in 0..3 {
                mean[i] += p[i];
            }
            let max = p[0].max(p[1]).max(p[2]);
            let min = p[0].min(p[1]).min(p[2]);
            saturation += max - min;
            if max >= 254.0 / 255.0 {
                clipped += 1;
            }
            if max <= 2.0 / 255.0 {
                crushed += 1;
            }
            let y = luma(*p);
            lo = lo.min(y);
            hi = hi.max(y);
            luma_sum += y;
            hist[((y.clamp(0.0, 1.0) * (BINS - 1) as f64).round()) as usize] += 1;
        }
        let at = |q: f64| {
            let want = (q * (pixels.len() - 1) as f64).round() as usize;
            let mut seen = 0;
            for (b, count) in hist.iter().enumerate() {
                seen += count;
                if seen > want {
                    return b as f64 / (BINS - 1) as f64;
                }
            }
            1.0
        };
        let light = linear_mean(pixels);
        Some(ColorStats {
            pixels: pixels.len(),
            mean: mean.map(|v| v / n),
            luma: Levels { min: lo, p5: at(0.05), median: at(0.5), p95: at(0.95), max: hi, mean: luma_sum / n },
            exposure: exposure_of(light),
            balance: balance_of(light),
            saturation: saturation / n,
            clipped: clipped as f64 / n,
            crushed: crushed as f64 / n,
        })
    }
}

/// Rec. 709 luma of a display-encoded colour.
fn luma(c: [f64; 3]) -> f64 {
    (0..3).map(|i| LUMA[i] * c[i]).sum()
}

/// The light a display value stands for (BT.1886, as the grade decodes it).
fn decode(v: f64) -> f64 {
    v.max(0.0).powf(DISPLAY_GAMMA)
}

/// The mean light of `pixels`, per channel.
fn linear_mean(pixels: &[[f64; 3]]) -> [f64; 3] {
    let mut sum = [0.0; 3];
    for p in pixels {
        for i in 0..3 {
            sum[i] += decode(p[i]);
        }
    }
    sum.map(|s| s / pixels.len() as f64)
}

/// Light in stops from mid grey.
fn exposure_of(light: [f64; 3]) -> f64 {
    (luma(light).max(FLOOR) / MID_GREY).log2()
}

/// The temperature and tint that turn a grey's light into `light`'s. The grade's white
/// balance scales red by 2^(t/2), green by 2^(-m/2) and blue by 2^(-t/2) (then all three
/// alike), so a grey becomes r/g = 2^((t+m)/2) and b/g = 2^((m-t)/2): solved, t = log2(r/b)
/// and m = log2(r·b/g²).
fn balance_of(light: [f64; 3]) -> Balance {
    let [r, g, b] = light.map(|v| v.max(FLOOR));
    Balance { temperature: (r / b).log2(), tint: (r * b / (g * g)).log2() }
}

/// What a match aims a region at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// Another region's light, per channel (its mean, decoded): its brightness and its cast.
    Light([f64; 3]),
    /// No cast, at the brightness the region already has: white balance alone.
    Neutral,
}

impl Target {
    /// Aim at a region with these statistics: its mean light.
    pub fn like(pixels: &[[f64; 3]]) -> Option<Target> {
        (!pixels.is_empty()).then(|| Target::Light(linear_mean(pixels)))
    }

    /// Aim at a colour as displayed (`#rrggbb`): the region's mean becomes it.
    pub fn colour(css: &str) -> Option<Target> {
        let hex = css.strip_prefix('#')?;
        let digits: Vec<u8> = match hex.len() {
            3 => hex.chars().map(|c| c.to_digit(16).map(|d| (d * 17) as u8)).collect::<Option<_>>()?,
            6 => (0..3).map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()).collect::<Option<_>>()?,
            _ => return None,
        };
        Some(Target::Light([0, 1, 2].map(|i| decode(digits[i] as f64 / 255.0))))
    }
}

/// A grade that makes a region look like its target, and what it makes of the region.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Matched {
    /// The clip's grade with `exposure`, `temperature` and `tint` solved; nothing else of
    /// it is changed.
    pub grade: Grade,
    /// The region through that grade, as kuma computes it.
    pub after: ColorStats,
    /// Whether it got there: within 2% of the target's light on every channel. A target
    /// past what the grade's ranges reach, or light that clipping holds back, is not.
    pub reached: bool,
}

/// Solve `grade`'s white balance and exposure so that `pixels` — the region as the clip
/// shows it *before* its grade — come out at `target` through the whole grade. The rest of
/// the grade stays as it is, its LUTs (`input`, `look`) included. Each step adds the
/// difference the light still shows, read in the grade's own units (as [`ColorStats`]
/// reads a cast), and tries again: exact in one step for a grade of white balance and
/// exposure alone, and a few steps more when its other settings bend the light after them.
pub fn match_light(grade: &Grade, input: Option<Lut>, look: Option<Lut>, pixels: &[[f64; 3]], target: Target) -> Option<Matched> {
    if pixels.is_empty() {
        return None;
    }
    let mut grading = Grading::new(grade, input, look);
    let graded = |g: &Grading| -> Vec<[f64; 3]> { pixels.iter().map(|p| g.apply(*p)).collect() };
    let (mut t, mut m, mut e) = (grade.temperature, grade.tint, grade.exposure);
    let goal = |now: [f64; 3]| match target {
        Target::Light(light) => light,
        Target::Neutral => [luma(now); 3],
    };
    let mut reached = false;
    for _ in 0..16 {
        grading.set_light(t, m, e);
        let now = linear_mean(&graded(&grading));
        let want = goal(now);
        reached = (0..3).all(|i| (now[i] - want[i]).abs() <= 0.02 * want[i].max(FLOOR));
        let k: [f64; 3] = std::array::from_fn(|i| want[i].max(FLOOR) / now[i].max(FLOOR));
        let step = balance_of(k);
        let de = if target == Target::Neutral { 0.0 } else { (luma(want).max(FLOOR) / luma(now).max(FLOOR)).log2() };
        // The validator's ranges: a grade solved past them could not be written.
        let next = ((t + step.temperature).clamp(-2.0, 2.0), (m + step.tint).clamp(-1.0, 1.0), (e + de).clamp(-5.0, 5.0));
        let moved = (next.0 - t).abs().max((next.1 - m).abs()).max((next.2 - e).abs());
        (t, m, e) = next;
        if moved < 1e-5 {
            break;
        }
    }
    // Three places: a hundredth of a stop is below what anyone sees, and a patch reads better.
    let round = |v: f64| (v * 1000.0).round() / 1000.0;
    let solved = Grade { temperature: round(t), tint: round(m), exposure: round(e), ..grade.clone() };
    grading.set_light(solved.temperature, solved.tint, solved.exposure);
    let after = ColorStats::of(&graded(&grading))?;
    Some(Matched { grade: solved, after, reached })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgb;

    fn encode(light: f64) -> f64 {
        light.powf(1.0 / DISPLAY_GAMMA)
    }

    #[test]
    fn a_flat_region_says_what_it_is() {
        let grey = vec![[encode(MID_GREY); 3]; 100];
        let s = ColorStats::of(&grey).unwrap();
        assert!(s.exposure.abs() < 1e-9 && s.balance.temperature.abs() < 1e-9 && s.balance.tint.abs() < 1e-9, "{s:?}");
        assert_eq!((s.clipped, s.crushed, s.saturation), (0.0, 0.0, 0.0));
        assert!((s.luma.median - encode(MID_GREY)).abs() < 1e-3);

        // Half white, half black: both ends counted, and the median between them.
        let split: Vec<_> = (0..100).map(|i| if i < 50 { [1.0; 3] } else { [0.0; 3] }).collect();
        let s = ColorStats::of(&split).unwrap();
        assert_eq!((s.clipped, s.crushed, s.luma.min, s.luma.max), (0.5, 0.5, 0.0, 1.0));
        assert!(ColorStats::of(&[]).is_none());
    }

    /// The balance is the grade's own: a grey through a white balance reads back as it.
    #[test]
    fn a_grey_through_a_white_balance_reads_back_as_that_balance() {
        for (t, m) in [(0.4, 0.0), (-0.3, 0.2), (0.7, -0.5)] {
            let g = Grade { temperature: t, tint: m, ..Grade::default() };
            let px = Grading::new(&g, None, None).apply([encode(MID_GREY); 3]);
            let b = ColorStats::of(&[px]).unwrap().balance;
            assert!((b.temperature - t).abs() < 1e-9 && (b.tint - m).abs() < 1e-9, "({t}, {m}) read as {b:?}");
        }
        // Exposure: a stop more light is a stop up.
        let lit = Grading::new(&Grade { exposure: 1.0, ..Grade::default() }, None, None).apply([encode(MID_GREY); 3]);
        assert!((ColorStats::of(&[lit]).unwrap().exposure - 1.0).abs() < 1e-9);
    }

    /// A shot that came out too blue and too dark, matched to the same grey shot right.
    #[test]
    fn a_match_brings_a_region_to_another_regions_light() {
        let reference = vec![[encode(0.2), encode(0.2), encode(0.2)]; 10];
        let wrong = Grade { temperature: -0.4, tint: 0.1, exposure: -0.8, ..Grade::default() };
        let off = Grading::new(&wrong, None, None);
        let shot: Vec<_> = reference.iter().map(|p| off.apply(*p)).collect();
        let m = match_light(&Grade::default(), None, None, &shot, Target::like(&reference).unwrap()).unwrap();
        assert!(m.reached, "{m:?}");
        assert!((m.grade.temperature - 0.4).abs() < 0.01 && (m.grade.tint + 0.1).abs() < 0.01 && (m.grade.exposure - 0.8).abs() < 0.01, "{:?}", m.grade);
        let want = ColorStats::of(&reference).unwrap();
        assert!((m.after.exposure - want.exposure).abs() < 0.03, "{:?} vs {:?}", m.after, want);

        // The grade's other settings stay, and the match works through them.
        let styled = Grade { gain: Rgb { r: 0.0, g: 0.0, b: 0.1 }, saturation: 0.8, contrast: 1.2, ..Grade::default() };
        let m = match_light(&styled, None, None, &shot, Target::like(&reference).unwrap()).unwrap();
        assert!(m.reached, "{m:?}");
        assert_eq!((m.grade.gain, m.grade.saturation, m.grade.contrast), (styled.gain, 0.8, 1.2));
    }

    #[test]
    fn neutral_takes_the_cast_away_and_leaves_the_light() {
        let warmer = Grade { temperature: 0.5, ..Grade::default() };
        let warm = Grading::new(&warmer, None, None);
        let wall = vec![warm.apply([encode(0.5); 3]); 10];
        let before = ColorStats::of(&wall).unwrap();
        let m = match_light(&Grade::default(), None, None, &wall, Target::Neutral).unwrap();
        assert!(m.reached && m.grade.exposure == 0.0, "{m:?}");
        assert!(m.after.balance.temperature.abs() < 0.03 && m.after.balance.tint.abs() < 0.03, "{:?}", m.after.balance);
        assert!((m.after.exposure - before.exposure).abs() < 0.05, "brightness kept: {} → {}", before.exposure, m.after.exposure);
    }

    #[test]
    fn a_target_out_of_reach_says_so() {
        // Clipped white cannot be made brighter, and nothing reaches a colour that bright.
        let white = vec![[1.0; 3]; 4];
        let m = match_light(&Grade::default(), None, None, &white, Target::Light([3.0; 3])).unwrap();
        assert!(!m.reached && m.grade.exposure <= 5.0);
    }

    #[test]
    fn a_colour_target_reads_hex() {
        assert_eq!(Target::colour("#fff"), Some(Target::Light([1.0; 3])));
        let Some(Target::Light(l)) = Target::colour("#808080") else { panic!() };
        assert!((l[0] - decode(128.0 / 255.0)).abs() < 1e-12);
        assert_eq!(Target::colour("808080"), None);
        assert_eq!(Target::colour("#80808"), None);
    }
}
