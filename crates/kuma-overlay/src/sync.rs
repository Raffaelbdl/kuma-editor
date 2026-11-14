//! Where one recording of a scene sits against another: a camera's own sound against the
//! mic that recorded the same words, found by cross-correlating the two.
//!
//! Both are decoded by ffmpeg to mono at a few kHz, high-passed so rumble and hum (which
//! both recordings share whatever the offset) do not pull the peak, normalised, and
//! correlated through an FFT. The peak is the offset; how far it stands over the next best
//! one, outside its own neighbourhood, is the confidence. At [`RATE`] the offset is to a
//! quarter of a millisecond, which is far finer than the frame a clip's `in` is rounded to.
//! Measured on real takes (a Lumix and a Boya mic, one to two minutes each): matched pairs
//! scored 2.6 to 5, unrelated pairs about 1.03, and the clocks drifted less than a
//! millisecond over a take, so one offset holds for the whole of one.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// The rate both recordings are decoded at: speech has its energy under 2 kHz.
pub const RATE: u32 = 4000;
/// The most samples the two may come to between them, at whatever rate: the FFT is the
/// next power of two over that, `2^23` floats twice over (about 130 MB). Past it the rate
/// halves, and halves again, down to 1 kHz — still a millisecond.
const MAX_SAMPLES: f64 = (1u64 << 23) as f64;
/// Under this the peak is not told apart from the noise: unrelated recordings score about
/// 1.03, and matched ones 2.6 and up.
pub const MIN_CONFIDENCE: f64 = 1.5;
/// How much either side of the peak is its own neighbourhood, seconds: the confidence is
/// the peak over the best correlation outside it.
const NEIGHBOURHOOD: f64 = 0.05;

/// Where `other` sits against `reference`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Alignment {
    /// Seconds: at `reference`'s time `t`, `other` is at `t + offset`. Positive when
    /// `other` started recording first.
    pub offset: f64,
    /// The peak over the next best correlation; [`MIN_CONFIDENCE`] and up is a match.
    pub confidence: f64,
    /// The rate it was found at, Hz: the offset is to a sample of it.
    pub rate: u32,
}

impl Alignment {
    pub fn is_match(&self) -> bool {
        self.confidence >= MIN_CONFIDENCE
    }
}

/// Align `other` to `reference`, two files whose first sound streams recorded the same
/// scene. `total` — their lengths added, seconds, as far as known — chooses the rate before
/// anything is decoded.
pub fn align(reference: &Path, other: &Path, total: f64, cancel: &AtomicBool) -> Result<Alignment, String> {
    let rate = [RATE, RATE / 2, RATE / 4].into_iter().find(|&r| total * r as f64 <= MAX_SAMPLES).unwrap_or(RATE / 4);
    // Two ffmpeg processes, side by side: neither waits on the other.
    let (a, b) = std::thread::scope(|s| {
        let a = s.spawn(|| decode(reference, rate, cancel));
        (a.join().unwrap_or_else(|_| Err("a decode panicked".into())), decode(other, rate, cancel))
    });
    correlate(a?, b?, rate).ok_or_else(|| format!("{} or {} is silent, so there is nothing to line up", reference.display(), other.display()))
}

/// The first sound stream of `src` as mono `f32` at `rate`, high-passed at 100 Hz.
fn decode(src: &Path, rate: u32, cancel: &AtomicBool) -> Result<Vec<f32>, String> {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(src)
        .args(["-map", "0:a:0", "-ac", "1", "-ar", &rate.to_string(), "-af", "highpass=f=100", "-f", "f32le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped");
    // Samples as they come, each read converted; a sample split across reads waits in `buf`.
    let mut samples = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut held = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".into());
        }
        let n = stdout.read(&mut buf[held..]).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let total = held + n;
        samples.extend(buf[..total].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        held = total % 4;
        buf.copy_within(total - held..total, 0);
    }
    let done = child.wait_with_output().map_err(|e| e.to_string())?;
    if !done.status.success() {
        let err = String::from_utf8_lossy(&done.stderr).trim().to_string();
        return Err(format!("{}: {}", src.display(), if err.is_empty() { "ffmpeg could not read its sound".into() } else { err }));
    }
    Ok(samples)
}

/// Cross-correlate two signals at `rate`: where `other` best matches `reference`. `None`
/// when either is empty or silent. Each signal becomes its own transform's buffer, so no
/// copy of it is held beside it.
pub fn correlate(reference: Vec<f32>, other: Vec<f32>, rate: u32) -> Option<Alignment> {
    let reference_len = reference.len();
    let n = (reference_len + other.len()).next_power_of_two();
    let (mut rr, mut or) = (normalised(reference, n)?, normalised(other, n)?);
    let (mut ri, mut oi) = (vec![0.0; n], vec![0.0; n]);
    // One table for the three transforms: the inverse's is its conjugate.
    let twiddles = twiddles(n);
    fft(&mut rr, &mut ri, &twiddles, false);
    fft(&mut or, &mut oi, &twiddles, false);
    // other × conj(reference): at lag k it is Σ other[t + k] · reference[t].
    for k in 0..n {
        let (a, b, c, d) = (or[k], oi[k], rr[k], -ri[k]);
        or[k] = a * c - b * d;
        oi[k] = a * d + b * c;
    }
    fft(&mut or, &mut oi, &twiddles, true);
    let c = &or;
    let peak = (0..n).max_by(|&a, &b| c[a].total_cmp(&c[b]))?;
    // Lags past `other`'s length wrap round to the end: those are `reference` starting first.
    let lag = if peak > n - reference_len { peak as i64 - n as i64 } else { peak as i64 };
    let near = (NEIGHBOURHOOD * rate as f64).ceil() as usize;
    let distance = |i: usize| i.abs_diff(peak).min(n - i.abs_diff(peak));
    let runner_up = (0..n).filter(|&i| distance(i) > near).map(|i| c[i].abs()).fold(0.0f32, f32::max);
    let confidence = if runner_up > 0.0 { (c[peak] / runner_up) as f64 } else { f64::INFINITY };
    Some(Alignment { offset: lag as f64 / rate as f64, confidence, rate })
}

/// `x` less its mean, over its deviation, in place, then padded with zeros to `n`; `None`
/// when it has no deviation (or is empty).
fn normalised(mut x: Vec<f32>, n: usize) -> Option<Vec<f32>> {
    if x.is_empty() {
        return None;
    }
    let mean = x.iter().map(|&v| v as f64).sum::<f64>() / x.len() as f64;
    let dev = (x.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    if dev <= 1e-9 {
        return None;
    }
    for v in &mut x {
        *v = ((*v as f64 - mean) / dev) as f32;
    }
    x.resize(n, 0.0);
    Some(x)
}

/// The forward transform's table for length `n`: `e^(-2πik/n)` for `k < n/2`, in `f64` so a
/// long transform does not accumulate its error.
fn twiddles(n: usize) -> Vec<(f32, f32)> {
    (0..n / 2)
        .map(|k| {
            let a = -std::f64::consts::TAU * k as f64 / n as f64;
            (a.cos() as f32, a.sin() as f32)
        })
        .collect()
}

/// An in-place radix-2 FFT of a power-of-two length, unscaled either way (the correlation
/// wants where its peak is and how tall against the rest, not its units). `twiddles` is the
/// forward table ([`twiddles`]); the inverse conjugates it.
fn fft(re: &mut [f32], im: &mut [f32], twiddles: &[(f32, f32)], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { -1.0 } else { 1.0 };
    let mut len = 2;
    while len <= n {
        let (half, step) = (len / 2, n / len);
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let (wr, wi) = twiddles[k * step];
                let wi = sign * wi;
                let (a, b) = (start + k, start + k + half);
                let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repeatable noise, xorshift: the tests need speech-like broadband, not a tone,
    /// whose correlation would peak once a period.
    fn noise(seed: u64, n: usize) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            })
            .collect()
    }

    #[test]
    fn the_fft_is_the_dft() {
        let x = noise(7, 16);
        let (mut re, mut im) = (x.clone(), vec![0.0; 16]);
        fft(&mut re, &mut im, &twiddles(16), false);
        for k in 0..16 {
            let (mut sr, mut si) = (0.0f64, 0.0f64);
            for (t, &v) in x.iter().enumerate() {
                let a = -std::f64::consts::TAU * (k * t) as f64 / 16.0;
                sr += v as f64 * a.cos();
                si += v as f64 * a.sin();
            }
            assert!((re[k] as f64 - sr).abs() < 1e-4 && (im[k] as f64 - si).abs() < 1e-4, "bin {k}");
        }
    }

    /// The mic starts 1.2345 s before the camera, both hear the scene, each its own hiss.
    #[test]
    fn finds_where_a_second_recording_sits() {
        let rate = RATE;
        let scene = noise(1, rate as usize * 20);
        let early = (1.2345 * rate as f64).round() as usize;
        let hiss = |seed, n| noise(seed, n).into_iter().map(|v| v * 0.8).collect::<Vec<_>>();
        let camera: Vec<f32> = scene[early..early + rate as usize * 12].iter().zip(hiss(2, rate as usize * 12)).map(|(a, b)| a + b).collect();
        let mic: Vec<f32> = scene.iter().zip(hiss(3, scene.len())).map(|(a, b)| a + b).collect();
        let found = correlate(camera.clone(), mic.clone(), rate).unwrap();
        assert!((found.offset - early as f64 / rate as f64).abs() < 0.5 / rate as f64, "{found:?}");
        assert!(found.is_match(), "{found:?}");
        // And the other way round: the camera seen from the mic is the opposite offset.
        let back = correlate(mic, camera, rate).unwrap();
        assert!((back.offset + found.offset).abs() < 0.5 / rate as f64, "{back:?}");
    }

    #[test]
    fn unrelated_recordings_do_not_match() {
        let found = correlate(noise(4, RATE as usize * 10), noise(5, RATE as usize * 15), RATE).unwrap();
        assert!(!found.is_match(), "{found:?}");
    }

    #[test]
    fn silence_has_nothing_to_line_up() {
        assert_eq!(correlate(vec![0.0; 100], noise(6, 100), RATE), None);
        assert_eq!(correlate(vec![], noise(6, 100), RATE), None);
    }
}
