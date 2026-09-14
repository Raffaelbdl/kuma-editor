//! Rational frame rate and the one place seconds become frames.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Frame rate as a rational number. `29.97` is `30000/1001`, never `29.97`.
///
/// Accepts three JSON spellings: `{ "num": 30000, "den": 1001 }`, an integer (`30`),
/// or one of the well-known NTSC floats (`29.97`, `23.976`, `59.94`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "FpsRepr", into = "FpsRepr")]
pub struct Fps {
    pub num: u32,
    pub den: u32,
}

/// Wire representation of [`Fps`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FpsRepr {
    Rational { num: u32, den: u32 },
    Number(f64),
}

impl From<FpsRepr> for Fps {
    fn from(r: FpsRepr) -> Self {
        match r {
            FpsRepr::Rational { num, den } => Fps::new(num, den.max(1)),
            FpsRepr::Number(n) => Fps::from_f64(n),
        }
    }
}

impl From<Fps> for FpsRepr {
    fn from(f: Fps) -> Self {
        FpsRepr::Rational { num: f.num, den: f.den }
    }
}

impl JsonSchema for Fps {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Fps".into()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut s = FpsRepr::json_schema(g);
        s.insert(
            "description".into(),
            "Frame rate. Either {num, den} or a number (30, 25, 29.97, 23.976, 59.94).".into(),
        );
        s
    }
}

impl Fps {
    /// Always stored reduced, so `Eq`/`Hash`/`Display` agree for equal rates.
    pub const fn new(num: u32, den: u32) -> Self {
        let g = gcd(num, den);
        if g == 0 { Fps { num, den } } else { Fps { num: num / g, den: den / g } }
    }

    /// Map a float to a rational, snapping the NTSC family to their exact 1001 forms.
    pub fn from_f64(n: f64) -> Self {
        const NTSC: [(f64, u32); 5] = [
            (23.976, 24000),
            (29.97, 30000),
            (59.94, 60000),
            (119.88, 120000),
            (47.952, 48000),
        ];
        for (approx, num) in NTSC {
            if (n - approx).abs() < 0.01 {
                return Fps { num, den: 1001 };
            }
        }
        // Generic fallback: 3 decimal places, reduced.
        Fps::new((n * 1000.0).round() as u32, 1000)
    }

    pub fn as_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Seconds → frame index, rounded to nearest. This is the only rounding site.
    pub fn frame(&self, secs: f64) -> i64 {
        (secs * self.num as f64 / self.den as f64).round() as i64
    }

    /// Frame span for a `[start, end)` range in seconds, as MLT expects it:
    /// `in` inclusive, `out` inclusive, `len = out - in + 1`. Never shorter than 1 frame.
    pub fn clip_frames(&self, start: f64, end: f64) -> ClipFrames {
        let in_ = self.frame(start);
        let out = (self.frame(end) - 1).max(in_);
        ClipFrames { in_, out }
    }

    /// Duration in seconds → frame count (at least 1).
    pub fn length(&self, secs: f64) -> i64 {
        self.frame(secs).max(1)
    }
}

impl Default for Fps {
    fn default() -> Self {
        Fps::new(30, 1)
    }
}

/// Inclusive frame range as MLT models it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipFrames {
    pub in_: i64,
    pub out: i64,
}

impl ClipFrames {
    pub fn len(&self) -> i64 {
        self.out - self.in_ + 1
    }
}

const fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntsc_snaps_to_1001() {
        assert_eq!(Fps::from_f64(29.97), Fps::new(30000, 1001));
        assert_eq!(Fps::from_f64(23.976), Fps::new(24000, 1001));
        assert_eq!(Fps::from_f64(30.0), Fps::new(30, 1));
        assert_eq!(Fps::from_f64(25.5), Fps { num: 51, den: 2 });
        assert_eq!(Fps::new(60000, 2002), Fps::new(30000, 1001));
    }

    #[test]
    fn deserializes_all_spellings() {
        let a: Fps = serde_json::from_str("30").unwrap();
        let b: Fps = serde_json::from_str("29.97").unwrap();
        let c: Fps = serde_json::from_str(r#"{"num":30000,"den":1001}"#).unwrap();
        assert_eq!(a, Fps::new(30, 1));
        assert_eq!(b, c);
    }

    #[test]
    fn clip_frames_are_inclusive_and_exclusive_end_is_respected() {
        let fps = Fps::new(30, 1);
        let f = fps.clip_frames(0.0, 1.0);
        assert_eq!((f.in_, f.out, f.len()), (0, 29, 30));
        let f = fps.clip_frames(2.0, 8.0);
        assert_eq!((f.in_, f.out, f.len()), (60, 239, 180));
        // Zero-length range still yields one frame rather than a negative span.
        let f = fps.clip_frames(1.0, 1.0);
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn ntsc_does_not_drift() {
        let fps = Fps::new(30000, 1001);
        // 10 minutes at 29.97 is 17982.017 frames → 17982; an integer 29.97 would give 17982
        // here too but drift by a frame every ~33 s in accumulated clip math.
        assert_eq!(fps.frame(600.0), 17982);
        // Accumulating 1000 one-second clips must equal one 1000-second clip.
        let per_clip: i64 = (0..1000).map(|i| fps.clip_frames(i as f64, i as f64 + 1.0).len()).sum();
        assert_eq!(per_clip, fps.clip_frames(0.0, 1000.0).len());
    }
}
