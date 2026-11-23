/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! 2x interpolation of 16-bit PCM by a half-band FIR filter (Kaiser-windowed
//! sinc), in polyphase form: every even output sample is an input sample, every
//! odd one the filter's value half way between two inputs. Used for HE-AAC's
//! AAC-LC core in software (no SBR decoder), so its quality matters: flat to
//! 0.45 of the input rate, images of that band at least ~74 dB down.

/// Taps of the half-sample phase: the filter is `4 * HALF - 1` taps long.
pub const TAPS: usize = 48;
const HALF: usize = TAPS / 2;
/// Kaiser window shape (about 75 dB of stopband).
const BETA: f64 = 7.3;

/// Zeroth-order modified Bessel function of the first kind.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// The half-sample phase: coefficient `k` weighs input `n + k - HALF + 1`
/// for the value at `n + 0.5`.
fn coefficients() -> [f32; TAPS] {
    let mut c = [0f64; TAPS];
    // Window over the whole filter: its taps run from -(2*HALF - 1) to
    // 2*HALF - 1 in output samples; this phase's taps sit at odd positions.
    let half_len = (2 * HALF) as f64;
    for (k, ck) in c.iter_mut().enumerate() {
        let t = k as f64 - HALF as f64 + 0.5; // input samples from the point
        let m = 2.0 * t; // output samples
        let sinc = (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t);
        let r = m / half_len;
        let w = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(BETA);
        *ck = sinc * w;
    }
    // Unity gain at DC.
    let sum: f64 = c.iter().sum();
    let mut out = [0f32; TAPS];
    for (o, v) in out.iter_mut().zip(c) {
        *o = (v / sum) as f32;
    }
    out
}

/// The interpolator's state for up to two channels. Its output lags the
/// input by `TAPS / 2` input frames (it starts with silence).
pub struct HalfBand {
    coefficients: [f32; TAPS],
    channels: usize,
    /// The last `TAPS` input samples per channel, twice over (a ring that
    /// can be read as one slice).
    history: [[f32; 2 * TAPS]; 2],
    pos: usize,
}

impl HalfBand {
    pub fn new(channels: usize) -> HalfBand {
        assert!((1..=2).contains(&channels));
        HalfBand {
            coefficients: coefficients(),
            channels,
            history: [[0.0; 2 * TAPS]; 2],
            pos: 0,
        }
    }

    /// Back to silence.
    pub fn reset(&mut self) {
        self.history = [[0.0; 2 * TAPS]; 2];
        self.pos = 0;
    }

    /// Interpolate interleaved S16_LE `input` to twice the frames, appended to
    /// `out` as interleaved S16_LE.
    pub fn process(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let ch = self.channels;
        out.reserve(input.len() * 2);
        for frame in input.chunks_exact(2 * ch) {
            let mut even = [0f32; 2];
            let mut odd = [0f32; 2];
            for c in 0..ch {
                let x = f32::from(i16::from_le_bytes([frame[2 * c], frame[2 * c + 1]]));
                let h = &mut self.history[c];
                h[self.pos] = x;
                h[self.pos + TAPS] = x;
                // The window: the last TAPS inputs, oldest first.
                let w = &h[self.pos + 1..self.pos + 1 + TAPS];
                // The input sample at the filter's centre, and the value half
                // a sample after it.
                even[c] = w[HALF - 1];
                odd[c] = w.iter().zip(&self.coefficients).map(|(a, b)| a * b).sum();
            }
            self.pos = (self.pos + 1) % TAPS;
            for v in [even, odd] {
                for &s in v.iter().take(ch) {
                    let s = s.round().clamp(-32768.0, 32767.0) as i16;
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Amplitude of `freq` (cycles per sample) in `x`, by correlation under
    /// a 4-term Blackman-Harris window (sidelobes -92 dB: the tone itself
    /// does not leak into the image's bin).
    fn amplitude(x: &[f64], freq: f64) -> f64 {
        use std::f64::consts::PI;
        let len = x.len() as f64;
        let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
        for (n, v) in x.iter().enumerate() {
            let q = 2.0 * PI * n as f64 / (len - 1.0);
            let w =
                0.35875 - 0.48829 * q.cos() + 0.14128 * (2.0 * q).cos() - 0.01168 * (3.0 * q).cos();
            let p = 2.0 * PI * freq * n as f64;
            re += w * v * p.cos();
            im += w * v * p.sin();
            wsum += w;
        }
        2.0 * (re * re + im * im).sqrt() / wsum
    }

    /// A sweep of sines through the interpolator: passband ripple up to 0.45
    /// of the input rate, and how far down each one's image is.
    #[test]
    fn sweep() {
        let n_in = 8192;
        let mut worst_image = f64::NEG_INFINITY;
        let (mut min_gain, mut max_gain) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut f = 0.005;
        while f <= 0.45 + 1e-9 {
            let mut hb = HalfBand::new(1);
            let mut input = Vec::new();
            for n in 0..n_in {
                let v = (20000.0 * (2.0 * std::f64::consts::PI * f * n as f64).sin()) as i16;
                input.extend_from_slice(&v.to_le_bytes());
            }
            let mut out = Vec::new();
            hb.process(&input, &mut out);
            let y: Vec<f64> = out
                .chunks(2)
                .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])))
                .skip(4 * TAPS) // the start-up transient
                .collect();
            let a = amplitude(&y, f / 2.0);
            let image = amplitude(&y, 0.5 - f / 2.0);
            let gain = 20.0 * (a / 20000.0).log10();
            min_gain = min_gain.min(gain);
            max_gain = max_gain.max(gain);
            worst_image = worst_image.max(20.0 * (image / a).log10());
            f += 0.005;
        }
        eprintln!(
            "half-band, {} taps per phase: passband 0..0.45 gain {:.4} to {:.4} dB (ripple {:.4} dB), worst image {:.1} dB",
            TAPS,
            min_gain,
            max_gain,
            max_gain - min_gain,
            worst_image
        );
        assert!(max_gain - min_gain < 0.05);
        assert!(worst_image < -70.0);
    }

    /// Even outputs are the inputs (delayed), stereo channels stay apart.
    #[test]
    fn passes_samples_through() {
        let mut hb = HalfBand::new(2);
        let mut input = Vec::new();
        for n in 0..200i16 {
            input.extend_from_slice(&(n * 100).to_le_bytes());
            input.extend_from_slice(&(-n * 50).to_le_bytes());
        }
        let mut out = Vec::new();
        hb.process(&input, &mut out);
        assert_eq!(out.len(), input.len() * 2);
        let s = |i: usize| i16::from_le_bytes([out[2 * i], out[2 * i + 1]]);
        let lag = HALF;
        for n in lag..200 {
            let k = (n - lag) as i16;
            assert_eq!(s(4 * n), k * 100);
            assert_eq!(s(4 * n + 1), -k * 50);
        }
    }
}
