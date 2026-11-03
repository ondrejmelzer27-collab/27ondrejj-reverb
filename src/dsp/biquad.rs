//! RBJ Audio EQ Cookbook biquads: coefficient design, analytic response and
//! Transposed Direct Form II state, all in `f64`.
//!
//! The coefficient constructors clamp their inputs ([`clamp_freq`], [`clamp_q`],
//! [`clamp_gain_db`]) so the resulting filter is always finite and stable, whatever
//! the caller passes (including NaN/inf). `sample_rate` itself must be sane (positive,
//! finite, ≥ 1 kHz); [`super::EqChain`] guarantees that.

use std::f64::consts::PI;

/// Lowest corner/center frequency a filter is designed for.
pub const MIN_FREQ_HZ: f64 = 10.0;
/// Highest corner/center frequency, as a fraction of the sample rate (just below
/// Nyquist, so the RBJ formulas stay well conditioned).
pub const MAX_FREQ_FRACTION: f64 = 0.49;
/// Q range accepted by the designers. The upper limit only guards against absurd
/// values (the plugin's maximum is 10); poles stay strictly inside the unit circle.
pub const MIN_Q: f64 = 0.05;
pub const MAX_Q: f64 = 50.0;
/// Gain range accepted by the shelf/peak designers (the plugin's range is ±18 dB).
pub const MAX_GAIN_DB: f64 = 48.0;
/// State values with a magnitude below this are flushed to zero (see
/// [`BiquadState::flush_denormals`]).
pub const DENORMAL_THRESHOLD: f64 = 1e-20;
/// Floor for reported magnitudes (|H|² is clamped to 10^(MIN_MAGNITUDE_DB/10)) so the
/// analytic response is always finite, even at a high-pass filter's DC zero.
pub const MIN_MAGNITUDE_DB: f64 = -200.0;

/// Clamp a design frequency to `[MIN_FREQ_HZ, MAX_FREQ_FRACTION · fs]` (NaN → min).
pub fn clamp_freq(freq_hz: f64, sample_rate: f64) -> f64 {
    let hi = (MAX_FREQ_FRACTION * sample_rate).max(MIN_FREQ_HZ);
    if freq_hz.is_nan() {
        MIN_FREQ_HZ
    } else {
        freq_hz.clamp(MIN_FREQ_HZ, hi)
    }
}

/// Clamp Q to `[MIN_Q, MAX_Q]` (NaN → 1/√2).
pub fn clamp_q(q: f64) -> f64 {
    if q.is_nan() {
        std::f64::consts::FRAC_1_SQRT_2
    } else {
        q.clamp(MIN_Q, MAX_Q)
    }
}

/// Clamp a gain to `±MAX_GAIN_DB` (NaN → 0 dB).
pub fn clamp_gain_db(gain_db: f64) -> f64 {
    if gain_db.is_nan() {
        0.0
    } else {
        gain_db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
    }
}

/// `|H|²` → dB, floored at [`MIN_MAGNITUDE_DB`] (also maps NaN to the floor).
pub fn power_to_db(magnitude_sq: f64) -> f64 {
    let floor = 10f64.powf(MIN_MAGNITUDE_DB / 10.0);
    10.0 * magnitude_sq.max(floor).log10()
}

/// `φ = sin²(ω/2)` for `freq_hz` at `sample_rate`, the argument of
/// [`Coeffs::magnitude_sq`].
pub fn phi(freq_hz: f64, sample_rate: f64) -> f64 {
    let s = (PI * freq_hz / sample_rate).sin();
    s * s
}

/// Normalized biquad coefficients (`a0 = 1`):
/// `H(z) = (b0 + b1 z⁻¹ + b2 z⁻²) / (1 + a1 z⁻¹ + a2 z⁻²)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    /// Pass-through.
    pub const IDENTITY: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Divide by `a0`. Division (not multiplication by `1/a0`) keeps `b == a` exactly
    /// when the raw coefficients are equal, so a 0 dB shelf/peak is exactly unity.
    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// `(cos ω0, α = sin ω0 / 2Q)` with clamped inputs.
    fn cos_alpha(sample_rate: f64, freq_hz: f64, q: f64) -> (f64, f64) {
        let w0 = 2.0 * PI * clamp_freq(freq_hz, sample_rate) / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        (cos_w0, sin_w0 / (2.0 * clamp_q(q)))
    }

    /// RBJ high-pass (`HPF`), `|H(f0)| = Q`.
    pub fn highpass(sample_rate: f64, freq_hz: f64, q: f64) -> Self {
        let (cos_w0, alpha) = Self::cos_alpha(sample_rate, freq_hz, q);
        let b0 = 0.5 * (1.0 + cos_w0);
        Self::normalized(
            b0,
            -(1.0 + cos_w0),
            b0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        )
    }

    /// RBJ `lowShelf`, Q form (`α = sin ω0 / 2Q`, `A = 10^(dB/40)`).
    pub fn low_shelf(sample_rate: f64, freq_hz: f64, gain_db: f64, q: f64) -> Self {
        let (cos_w0, alpha) = Self::cos_alpha(sample_rate, freq_hz, q);
        let a = 10f64.powf(clamp_gain_db(gain_db) / 40.0);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let (ap1, am1) = (a + 1.0, a - 1.0);
        Self::normalized(
            a * (ap1 - am1 * cos_w0 + two_sqrt_a_alpha),
            2.0 * a * (am1 - ap1 * cos_w0),
            a * (ap1 - am1 * cos_w0 - two_sqrt_a_alpha),
            ap1 + am1 * cos_w0 + two_sqrt_a_alpha,
            -2.0 * (am1 + ap1 * cos_w0),
            ap1 + am1 * cos_w0 - two_sqrt_a_alpha,
        )
    }

    /// RBJ `highShelf`, Q form (`α = sin ω0 / 2Q`, `A = 10^(dB/40)`).
    pub fn high_shelf(sample_rate: f64, freq_hz: f64, gain_db: f64, q: f64) -> Self {
        let (cos_w0, alpha) = Self::cos_alpha(sample_rate, freq_hz, q);
        let a = 10f64.powf(clamp_gain_db(gain_db) / 40.0);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let (ap1, am1) = (a + 1.0, a - 1.0);
        Self::normalized(
            a * (ap1 + am1 * cos_w0 + two_sqrt_a_alpha),
            -2.0 * a * (am1 + ap1 * cos_w0),
            a * (ap1 + am1 * cos_w0 - two_sqrt_a_alpha),
            ap1 - am1 * cos_w0 + two_sqrt_a_alpha,
            2.0 * (am1 - ap1 * cos_w0),
            ap1 - am1 * cos_w0 - two_sqrt_a_alpha,
        )
    }

    /// RBJ `peakingEQ` (`|H(f0)| = A²`, i.e. exactly `gain_db` at the center).
    pub fn peaking(sample_rate: f64, freq_hz: f64, gain_db: f64, q: f64) -> Self {
        let (cos_w0, alpha) = Self::cos_alpha(sample_rate, freq_hz, q);
        let a = 10f64.powf(clamp_gain_db(gain_db) / 40.0);
        Self::normalized(
            1.0 + alpha * a,
            -2.0 * cos_w0,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w0,
            1.0 - alpha / a,
        )
    }

    /// Linear interpolation `a + (b − a)·t`. For `t ∈ [0, 1]` and two stable designs
    /// the result is stable too (the (a1, a2) stability triangle is convex).
    #[inline(always)]
    pub fn lerp(a: &Coeffs, b: &Coeffs, t: f64) -> Coeffs {
        Coeffs {
            b0: a.b0 + (b.b0 - a.b0) * t,
            b1: a.b1 + (b.b1 - a.b1) * t,
            b2: a.b2 + (b.b2 - a.b2) * t,
            a1: a.a1 + (b.a1 - a.a1) * t,
            a2: a.a2 + (b.a2 - a.a2) * t,
        }
    }

    /// `|H(e^{jω})|²` as a function of `φ = sin²(ω/2)` (see [`phi`]):
    ///
    /// `|B|² = (b0+b1+b2)² − 4(b0b1 + 4b0b2 + b1b2)φ + 16 b0b2 φ²`, likewise for `A`
    /// with `a0 = 1`. This form avoids the cancellation of the `cos ω` expansion at low
    /// frequencies (an HPF's numerator is exactly `16 b0² φ²`). Allocation-free, one
    /// division.
    #[inline]
    pub fn magnitude_sq(&self, phi: f64) -> f64 {
        let Coeffs { b0, b1, b2, a1, a2 } = *self;
        let bs = b0 + b1 + b2;
        let num =
            bs * bs - 4.0 * (b0 * b1 + 4.0 * b0 * b2 + b1 * b2) * phi + 16.0 * b0 * b2 * phi * phi;
        let as_ = 1.0 + a1 + a2;
        let den = as_ * as_ - 4.0 * (a1 + 4.0 * a2 + a1 * a2) * phi + 16.0 * a2 * phi * phi;
        num.max(0.0) / den.max(f64::MIN_POSITIVE)
    }

    /// Analytic magnitude in dB at `freq_hz` (evaluates `|H(e^{jω})|`, ω = 2πf/fs),
    /// floored at [`MIN_MAGNITUDE_DB`].
    pub fn magnitude_db(&self, freq_hz: f64, sample_rate: f64) -> f64 {
        power_to_db(self.magnitude_sq(phi(freq_hz, sample_rate)))
    }

    /// Complex response `H(e^{jω})` as `(re, im)`, given `cos ω, sin ω, cos 2ω, sin 2ω`.
    #[inline]
    pub fn response(&self, cos_w: f64, sin_w: f64, cos_2w: f64, sin_2w: f64) -> (f64, f64) {
        let nr = self.b0 + self.b1 * cos_w + self.b2 * cos_2w;
        let ni = -(self.b1 * sin_w + self.b2 * sin_2w);
        let dr = 1.0 + self.a1 * cos_w + self.a2 * cos_2w;
        let di = -(self.a1 * sin_w + self.a2 * sin_2w);
        let den = (dr * dr + di * di).max(f64::MIN_POSITIVE);
        ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den)
    }
}

/// One channel of Transposed Direct Form II state (`f64`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BiquadState {
    s1: f64,
    s2: f64,
}

impl BiquadState {
    /// Process one sample (TDF-II):
    /// `y = b0·x + s1; s1 = b1·x − a1·y + s2; s2 = b2·x − a2·y`.
    #[inline(always)]
    pub fn tick(&mut self, c: &Coeffs, x: f64) -> f64 {
        let y = c.b0 * x + self.s1;
        self.s1 = c.b1 * x - c.a1 * y + self.s2;
        self.s2 = c.b2 * x - c.a2 * y;
        y
    }

    /// Flush tiny state values to exactly zero, and recover from non-finite state.
    ///
    /// Called once per processed segment (a whole block, or a 32-sample sub-block while
    /// a de-click ramp runs), not per sample. That is enough for `f64` state: values
    /// are flushed below 1e-20, while `f64` only becomes subnormal below ~2.2e-308.
    /// A decaying filter cannot cross those ~288 decades inside one block unless its
    /// decay is so fast that it also crosses the 16 decades of the subnormal range (to
    /// exact zero) within a few samples, so silence never leaves the state lingering
    /// in subnormal numbers. Once the state is zero and the input is silent, the output
    /// is exactly 0.0.
    ///
    /// A NaN/inf state (only possible after NaN/inf input) is reset so a single bad
    /// input sample cannot poison the filter forever.
    #[inline]
    pub fn flush_denormals(&mut self) {
        self.s1 = flush(self.s1);
        self.s2 = flush(self.s2);
    }

    /// `true` if both state values are exactly zero.
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.s1 == 0.0 && self.s2 == 0.0
    }

    /// Largest absolute state value.
    #[inline]
    pub fn max_abs(&self) -> f64 {
        self.s1.abs().max(self.s2.abs())
    }

    /// Clear the delay line.
    #[inline]
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Raw state values `(s1, s2)` (for tests and diagnostics).
    pub fn values(&self) -> (f64, f64) {
        (self.s1, self.s2)
    }
}

#[inline(always)]
fn flush(s: f64) -> f64 {
    // NaN fails the comparison, so it is reset along with tiny values and ±inf.
    if s.abs() >= DENORMAL_THRESHOLD && s.is_finite() {
        s
    } else {
        0.0
    }
}
