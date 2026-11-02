//! Microphone noise suppression: spectral subtraction on 20 ms frames.
//!
//! Each half frame (10 ms) a 20 ms window goes through an FFT; every bin's
//! noise level is tracked (falling fast, rising slowly, so steady noise is
//! learnt and speech is not), and the bin is turned down by how much of it is
//! noise. Windows overlap by half and add back up exactly, so with nothing to
//! remove the output is the input, 10 ms late. Bins below 80 Hz (hum, desk
//! knocks) are always turned down.

use crate::FRAME_SIZE;

const HOP: usize = FRAME_SIZE / 2;
const WINDOW: usize = FRAME_SIZE;
const FFT: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    Off,
    Low,
    Medium,
    High,
}

impl Strength {
    pub const ALL: [Strength; 4] = [Strength::Off, Strength::Low, Strength::Medium, Strength::High];

    /// Over-subtraction and the gain floor (how far a noise-only bin goes down).
    fn params(self) -> (f32, f32) {
        match self {
            Strength::Off => (0.0, 1.0),
            Strength::Low => (1.3, 0.35),
            Strength::Medium => (2.0, 0.15),
            Strength::High => (3.0, 0.05),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Strength::Off => "off",
            Strength::Low => "low",
            Strength::Medium => "medium",
            Strength::High => "high",
        }
    }

    pub fn from_name(s: &str) -> Option<Strength> {
        Strength::ALL.into_iter().find(|x| x.name() == s)
    }
}

#[derive(Clone, Copy, Default)]
struct Complex {
    re: f32,
    im: f32,
}

/// An in-place radix-2 FFT of a fixed size.
struct Fft {
    twiddles: Vec<Complex>,
    bits: u32,
}

impl Fft {
    fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two());
        let twiddles = (0..n / 2)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                Complex { re: a.cos() as f32, im: a.sin() as f32 }
            })
            .collect();
        Fft { twiddles, bits: n.trailing_zeros() }
    }

    fn run(&self, data: &mut [Complex], inverse: bool) {
        let n = data.len();
        for i in 0..n {
            let j = i.reverse_bits() >> (usize::BITS - self.bits);
            if j > i {
                data.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let mut w = self.twiddles[k * step];
                    if inverse {
                        w.im = -w.im;
                    }
                    let a = data[start + k];
                    let b = data[start + k + len / 2];
                    let t = Complex { re: b.re * w.re - b.im * w.im, im: b.re * w.im + b.im * w.re };
                    data[start + k] = Complex { re: a.re + t.re, im: a.im + t.im };
                    data[start + k + len / 2] = Complex { re: a.re - t.re, im: a.im - t.im };
                }
            }
            len *= 2;
        }
        if inverse {
            let s = 1.0 / n as f32;
            for c in data.iter_mut() {
                c.re *= s;
                c.im *= s;
            }
        }
    }
}

pub struct Denoiser {
    pub strength: Strength,
    fft: Fft,
    window: Vec<f32>,
    /// The last window's worth of input.
    history: Vec<f32>,
    /// The second half of the last window's output, to add to the next.
    tail: Vec<f32>,
    /// Each bin's power averaged over about 60 ms: the raw power of noise
    /// jumps around too much to compare with.
    smooth: Vec<f32>,
    noise: Vec<f32>,
    gains: Vec<f32>,
    frames_seen: u32,
    buf: Vec<Complex>,
}

impl Denoiser {
    pub fn new(strength: Strength) -> Denoiser {
        // sqrt of a periodic Hann: analysis and synthesis together make a Hann,
        // whose halves overlapping by HOP sum to exactly 1.
        let window = (0..WINDOW)
            .map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / WINDOW as f64).cos()).sqrt() as f32)
            .collect();
        let bins = FFT / 2 + 1;
        Denoiser {
            strength,
            fft: Fft::new(FFT),
            window,
            history: vec![0.0; WINDOW],
            tail: vec![0.0; HOP],
            smooth: vec![0.0; bins],
            noise: vec![0.0; bins],
            gains: vec![1.0; bins],
            frames_seen: 0,
            buf: vec![Complex::default(); FFT],
        }
    }

    /// A 20 ms frame in place (10 ms of delay). Off passes it through untouched.
    pub fn process(&mut self, frame: &mut [i16]) {
        if self.strength == Strength::Off || frame.len() != FRAME_SIZE {
            return;
        }
        let input: Vec<f32> = frame.iter().map(|&s| s as f32 / 32768.0).collect();
        let mut out = Vec::with_capacity(FRAME_SIZE);
        for hop in input.chunks(HOP) {
            self.history.copy_within(HOP.., 0);
            self.history[WINDOW - HOP..].copy_from_slice(hop);
            out.extend(self.hop());
        }
        for (s, o) in frame.iter_mut().zip(out) {
            *s = (o * 32768.0).clamp(-32768.0, 32767.0) as i16;
        }
    }

    fn hop(&mut self) -> Vec<f32> {
        let (over, floor) = self.strength.params();
        for (i, c) in self.buf.iter_mut().enumerate() {
            *c = Complex { re: if i < WINDOW { self.history[i] * self.window[i] } else { 0.0 }, im: 0.0 };
        }
        self.fft.run(&mut self.buf, false);

        let bins = FFT / 2 + 1;
        let hz_per_bin = crate::SAMPLE_RATE as f32 / FFT as f32;
        self.frames_seen = self.frames_seen.saturating_add(1);
        for k in 0..bins {
            let c = self.buf[k];
            let power = c.re * c.re + c.im * c.im + 1e-12;
            let smooth = &mut self.smooth[k];
            *smooth = if self.frames_seen == 1 { power } else { 0.8 * *smooth + 0.2 * power };
            let smooth = *smooth;
            // The noise floor follows the smoothed power's minimum: down at
            // once, up about 2 dB a second, so a held vowel is not learnt.
            let noise = &mut self.noise[k];
            *noise = if self.frames_seen <= 10 || smooth < *noise { smooth } else { (*noise * 1.005).min(smooth) };
            // The minimum of a fluctuating power sits below its mean.
            const BIAS: f32 = 1.5;
            let mut g = (1.0 - over * BIAS * *noise / smooth).max(floor);
            if (k as f32) * hz_per_bin < 80.0 {
                g = floor;
            }
            // Open at once, close smoothly: no clicks, no chopped word ends.
            let prev = self.gains[k];
            let g = if g > prev { g } else { 0.6 * prev + 0.4 * g };
            self.gains[k] = g;
        }
        for k in 0..bins {
            let g = self.gains[k];
            self.buf[k].re *= g;
            self.buf[k].im *= g;
            if k > 0 && k < FFT / 2 {
                // Keep the spectrum conjugate-symmetric, the output real.
                self.buf[FFT - k] = Complex { re: self.buf[k].re, im: -self.buf[k].im };
            }
        }
        self.fft.run(&mut self.buf, true);

        let mut out = vec![0.0f32; HOP];
        for i in 0..HOP {
            out[i] = self.tail[i] + self.buf[i].re * self.window[i];
        }
        for i in 0..HOP {
            self.tail[i] = self.buf[HOP + i].re * self.window[HOP + i];
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed as f64 / u64::MAX as f64 * 2.0 - 1.0) as f32
    }

    fn power(x: &[i16]) -> f64 {
        x.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / x.len() as f64
    }

    #[test]
    fn fft_round_trips() {
        let f = Fft::new(16);
        let orig: Vec<Complex> = (0..16).map(|i| Complex { re: (i as f32 * 0.7).sin(), im: 0.0 }).collect();
        let mut d = orig.clone();
        f.run(&mut d, false);
        f.run(&mut d, true);
        for (a, b) in orig.iter().zip(&d) {
            assert!((a.re - b.re).abs() < 1e-5 && b.im.abs() < 1e-5);
        }
    }

    #[test]
    fn off_is_untouched() {
        let mut d = Denoiser::new(Strength::Off);
        let mut f: Vec<i16> = (0..FRAME_SIZE as i16).collect();
        let before = f.clone();
        d.process(&mut f);
        assert_eq!(f, before);
    }

    #[test]
    fn steady_noise_goes_down_and_a_tone_stays() {
        let mut d = Denoiser::new(Strength::Medium);
        let mut seed = 0x1234_5678_9abc_def0u64;
        // Two seconds of noise alone: learnt and turned down.
        let mut last_noise = 0.0;
        for _ in 0..100 {
            let mut f: Vec<i16> = (0..FRAME_SIZE).map(|_| (noise(&mut seed) * 3000.0) as i16).collect();
            let before = power(&f);
            d.process(&mut f);
            last_noise = power(&f) / before;
        }
        assert!(last_noise < 0.1, "noise kept {:.3} of its power", last_noise);

        // Then speech-like tone over the same noise: the tone gets through.
        let mut kept = Vec::new();
        let mut t = 0usize;
        for _ in 0..25 {
            let mut f: Vec<i16> = (0..FRAME_SIZE)
                .map(|_| {
                    t += 1;
                    let tone = (t as f32 / 48000.0 * 440.0 * std::f32::consts::TAU).sin() * 12000.0;
                    (tone + noise(&mut seed) * 3000.0) as i16
                })
                .collect();
            let before = power(&f);
            d.process(&mut f);
            kept.push(power(&f) / before);
        }
        let tail = kept[10..].iter().sum::<f64>() / 15.0;
        assert!(tail > 0.6, "tone kept {tail:.3} of the power");
    }

    #[test]
    fn hum_is_removed() {
        let mut d = Denoiser::new(Strength::Low);
        let mut last = 0.0;
        let mut t = 0usize;
        for _ in 0..50 {
            let mut f: Vec<i16> = (0..FRAME_SIZE)
                .map(|_| {
                    t += 1;
                    ((t as f32 / 48000.0 * 50.0 * std::f32::consts::TAU).sin() * 10000.0) as i16
                })
                .collect();
            let before = power(&f);
            d.process(&mut f);
            last = power(&f) / before;
        }
        assert!(last < 0.2, "50 Hz hum kept {last:.3}");
    }
}
