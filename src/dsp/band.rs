//! Band implementations used by [`super::EqChain`] (not public API):
//!
//! - [`ShelfPeakBand`]: one RBJ shelf/peak biquad whose bypass is de-clicked by an
//!   "amount" ramp that scales the effective gain (0 dB = unity).
//! - [`HpfBand`]: the high-pass with a 12 dB/oct path (1 biquad) and a 24 dB/oct path
//!   (2 biquads), crossfaded against each other (slope change) and against the dry
//!   signal (bypass).

use std::f64::consts::FRAC_1_SQRT_2;

use super::biquad::{self, BiquadState, Coeffs};
use super::{BandSettings, HpfSettings, HpfSlope};

/// Q of the 12 dB/oct (2nd-order Butterworth) high-pass.
pub(super) const HPF12_Q: f64 = FRAC_1_SQRT_2;
/// Qs of the two sections of the 24 dB/oct (4th-order Butterworth) high-pass:
/// `1 / (2 cos(π/8))` and `1 / (2 cos(3π/8))`.
pub(super) const HPF24_Q1: f64 = 0.541_196_100_146_197;
pub(super) const HPF24_Q2: f64 = 1.306_562_964_876_376_6;

/// A unity (0 dB) band keeps running until its leftover state has decayed below this,
/// then the state is dropped and the band is skipped. Running out the tail (instead of
/// skipping the instant the gain reaches 0 dB) avoids a small step when a gain is
/// smoothed to 0 dB or a bypass ramp ends; dropping < 1e-12 (−240 dBFS) is inaudible.
/// With unity coefficients the tail decays on its own (independently of the input),
/// so this always happens within the filter's decay time.
const RETIRE_THRESHOLD: f64 = 1e-12;

/// Smoothstep `3r² − 2r³`: maps 0 → 0 and 1 → 1 exactly, with zero slope at both ends.
/// Applied to the HPF crossfade weights so the gain envelope has no kink where a
/// crossfade starts or ends (equal-gain: the two weights still sum to 1).
#[inline(always)]
pub(super) fn smoothstep(r: f64) -> f64 {
    r * r * (3.0 - 2.0 * r)
}

/// Linear 0↔1 ramp, advanced once per de-click sub-block.
///
/// Within a sub-block the value moves linearly from `start` to `value` (used for
/// per-sample crossfade weights); coefficient designs use `value`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ramp {
    start: f64,
    value: f64,
    target: f64,
}

impl Ramp {
    pub(super) fn new(value: f64) -> Self {
        Self {
            start: value,
            value,
            target: value,
        }
    }

    /// Value at the end of the current sub-block (the current value when static).
    #[inline]
    pub(super) fn value(&self) -> f64 {
        self.value
    }

    /// Value at the start of the current sub-block.
    #[inline]
    pub(super) fn start(&self) -> f64 {
        self.start
    }

    #[inline]
    pub(super) fn set_target(&mut self, target: f64) {
        self.target = target;
    }

    /// `true` while the value has not reached the target.
    #[inline]
    pub(super) fn is_moving(&self) -> bool {
        self.value != self.target
    }

    /// `true` if the ramp sits at `v` and is not interpolating within a sub-block.
    #[inline]
    pub(super) fn is_settled_at(&self, v: f64) -> bool {
        self.start == v && self.value == v && self.target == v
    }

    /// Jump to the target (no ramp).
    #[inline]
    pub(super) fn snap(&mut self) {
        self.start = self.target;
        self.value = self.target;
    }

    /// Mark the end of a sub-block without moving (start = value).
    #[inline]
    pub(super) fn settle(&mut self) {
        self.start = self.value;
    }

    /// Begin a new sub-block: step `value` towards the target by `step`. Returns
    /// whether `value` changed. Snaps onto the target on the last step so a ramp of
    /// `N` sub-blocks ends after exactly `N` steps despite rounding.
    #[inline]
    pub(super) fn advance(&mut self, step: f64) -> bool {
        self.start = self.value;
        let delta = self.target - self.value;
        if delta == 0.0 {
            return false;
        }
        self.value = if delta.abs() <= step * (1.0 + 1e-9) {
            self.target
        } else {
            self.value + step.copysign(delta)
        };
        true
    }
}

/// Which RBJ design a [`ShelfPeakBand`] uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShelfKind {
    LowShelf,
    Peak,
    HighShelf,
}

/// Low shelf, mid peak or high shelf: one stereo RBJ biquad.
#[derive(Debug, Clone)]
pub(super) struct ShelfPeakBand {
    kind: ShelfKind,
    settings: BandSettings,
    /// 1 = engaged, 0 = bypassed. Scales the gain: effective gain = amount · gain_db.
    pub(super) amount: Ramp,
    /// Clamped `[freq, q, effective gain]` that `coeffs` were designed for.
    design: [f64; 3],
    pub(super) coeffs: Coeffs,
    /// Design at the start of the current bypass-ramp sub-block. While the amount
    /// ramps, the coefficients are interpolated per sample from `prev_coeffs` to
    /// `coeffs` across the sub-block, so the per-sub-block redesigns do not step.
    pub(super) prev_coeffs: Coeffs,
    pub(super) state: [BiquadState; 2],
    #[cfg(test)]
    pub(super) recomputes: usize,
}

impl ShelfPeakBand {
    pub(super) fn new(kind: ShelfKind, settings: &BandSettings, sample_rate: f64) -> Self {
        let mut band = Self {
            kind,
            settings: *settings,
            amount: Ramp::new(if settings.bypass { 0.0 } else { 1.0 }),
            // NaN never compares equal, so the first `update_coeffs` always designs.
            design: [f64::NAN; 3],
            coeffs: Coeffs::IDENTITY,
            prev_coeffs: Coeffs::IDENTITY,
            state: [BiquadState::default(); 2],
            #[cfg(test)]
            recomputes: 0,
        };
        band.update_coeffs(sample_rate);
        band.prev_coeffs = band.coeffs;
        band
    }

    /// Begin a de-click sub-block: step the amount and, if it moved, redesign (the
    /// sub-block then interpolates `prev_coeffs` → `coeffs`).
    pub(super) fn advance_ramp(&mut self, step: f64, sample_rate: f64) {
        self.prev_coeffs = self.coeffs;
        if self.amount.advance(step) {
            self.update_coeffs(sample_rate);
        }
    }

    /// Whether the current ramp sub-block interpolates the coefficients.
    #[inline]
    pub(super) fn interpolating(&self) -> bool {
        self.amount.start() != self.amount.value()
    }

    /// The coefficients are exactly unity (effective gain 0 dB).
    #[inline]
    pub(super) fn is_unity(&self) -> bool {
        self.design[2] == 0.0
    }

    /// Whether the band has to run: non-unity coefficients, or a unity band whose tail
    /// has not decayed yet. A unity band with zero state is skipped (bit-exact identity).
    #[inline]
    pub(super) fn needs_processing(&self) -> bool {
        !(self.is_unity() && !self.interpolating() && self.state.iter().all(BiquadState::is_zero))
    }

    /// New target settings. Bypass changes only retarget the amount ramp; the design
    /// is refreshed only if the clamped (freq, q, effective gain) changed.
    pub(super) fn set(&mut self, settings: &BandSettings, sample_rate: f64) {
        if *settings == self.settings {
            return;
        }
        self.settings = *settings;
        self.amount
            .set_target(if settings.bypass { 0.0 } else { 1.0 });
        if settings.gain_db == 0.0 {
            // The amount has no audible effect at 0 dB: no need to ramp.
            self.amount.snap();
        }
        self.update_coeffs(sample_rate);
    }

    /// Redesign the biquad if the clamped (freq, q, amount-scaled gain) changed.
    pub(super) fn update_coeffs(&mut self, sample_rate: f64) {
        let freq = biquad::clamp_freq(self.settings.freq_hz, sample_rate);
        let q = biquad::clamp_q(self.settings.q);
        let gain_db = biquad::clamp_gain_db(self.settings.gain_db) * self.amount.value();
        let design = [freq, q, gain_db];
        if design == self.design {
            return;
        }
        self.design = design;
        self.coeffs = match self.kind {
            ShelfKind::LowShelf => Coeffs::low_shelf(sample_rate, freq, gain_db, q),
            ShelfKind::Peak => Coeffs::peaking(sample_rate, freq, gain_db, q),
            ShelfKind::HighShelf => Coeffs::high_shelf(sample_rate, freq, gain_db, q),
        };
        #[cfg(test)]
        {
            self.recomputes += 1;
        }
    }

    /// End-of-segment housekeeping: denormal flush, and retire a decayed unity tail.
    pub(super) fn end_segment(&mut self) {
        let unity = self.is_unity() && !self.interpolating();
        for s in &mut self.state {
            s.flush_denormals();
            if unity && s.max_abs() < RETIRE_THRESHOLD {
                s.reset();
            }
        }
    }

    pub(super) fn reset_state(&mut self) {
        self.state = [BiquadState::default(); 2];
    }

    /// `|H|²` of the current effective coefficients (bypassed/0 dB → 1).
    #[inline]
    pub(super) fn magnitude_sq(&self, phi: f64) -> f64 {
        if self.is_unity() {
            1.0
        } else {
            self.coeffs.magnitude_sq(phi)
        }
    }
}

/// High-pass band: 12 dB/oct and 24 dB/oct Butterworth paths plus dry crossfade.
///
/// While the HPF is engaged (not fully bypassed) both slope paths run so the inactive
/// one is always warm and a slope change is a click-free crossfade between two
/// steady-state signals (1–2 extra biquads per channel). When fully bypassed it is
/// skipped and its state cleared; on un-bypass the paths start from zero state, whose
/// output starts out equal to the input (≈ b0·x), so the dry→wet crossfade is smooth.
#[derive(Debug, Clone)]
pub(super) struct HpfBand {
    settings: HpfSettings,
    /// Clamped cutoff the coefficients were designed for.
    design_freq: f64,
    pub(super) c12: Coeffs,
    pub(super) c24a: Coeffs,
    pub(super) c24b: Coeffs,
    pub(super) s12: [BiquadState; 2],
    pub(super) s24a: [BiquadState; 2],
    pub(super) s24b: [BiquadState; 2],
    /// 1 = filtered, 0 = dry (bypassed).
    pub(super) wet: Ramp,
    /// 0 = 12 dB/oct path, 1 = 24 dB/oct path.
    pub(super) mix24: Ramp,
    #[cfg(test)]
    pub(super) recomputes: usize,
}

fn slope_target(slope: HpfSlope) -> f64 {
    match slope {
        HpfSlope::Db12 => 0.0,
        HpfSlope::Db24 => 1.0,
    }
}

impl HpfBand {
    pub(super) fn new(settings: &HpfSettings, sample_rate: f64) -> Self {
        let mut band = Self {
            settings: *settings,
            design_freq: f64::NAN,
            c12: Coeffs::IDENTITY,
            c24a: Coeffs::IDENTITY,
            c24b: Coeffs::IDENTITY,
            s12: [BiquadState::default(); 2],
            s24a: [BiquadState::default(); 2],
            s24b: [BiquadState::default(); 2],
            wet: Ramp::new(if settings.bypass { 0.0 } else { 1.0 }),
            mix24: Ramp::new(slope_target(settings.slope)),
            #[cfg(test)]
            recomputes: 0,
        };
        band.update_coeffs(sample_rate);
        band
    }

    /// New target settings: bypass/slope retarget the crossfade ramps, a cutoff change
    /// redesigns the three biquads.
    pub(super) fn set(&mut self, settings: &HpfSettings, sample_rate: f64) {
        if *settings == self.settings {
            return;
        }
        self.settings = *settings;
        self.wet.set_target(if settings.bypass { 0.0 } else { 1.0 });
        self.mix24.set_target(slope_target(settings.slope));
        if self.wet.is_settled_at(0.0) {
            // Fully bypassed: the slope is inaudible, switch it without a crossfade.
            self.mix24.snap();
        }
        self.update_coeffs(sample_rate);
    }

    fn update_coeffs(&mut self, sample_rate: f64) {
        let freq = biquad::clamp_freq(self.settings.freq_hz, sample_rate);
        if freq == self.design_freq {
            return;
        }
        self.design_freq = freq;
        self.c12 = Coeffs::highpass(sample_rate, freq, HPF12_Q);
        self.c24a = Coeffs::highpass(sample_rate, freq, HPF24_Q1);
        self.c24b = Coeffs::highpass(sample_rate, freq, HPF24_Q2);
        #[cfg(test)]
        {
            self.recomputes += 1;
        }
    }

    pub(super) fn flush_denormals(&mut self) {
        for s in self
            .s12
            .iter_mut()
            .chain(self.s24a.iter_mut())
            .chain(self.s24b.iter_mut())
        {
            s.flush_denormals();
        }
    }

    pub(super) fn reset_state(&mut self) {
        self.s12 = [BiquadState::default(); 2];
        self.s24a = [BiquadState::default(); 2];
        self.s24b = [BiquadState::default(); 2];
    }

    /// `|H|²` of the current effective response at `ω` (`phi = sin²(ω/2)`).
    pub(super) fn magnitude_sq(&self, phi: f64, w: f64) -> f64 {
        let wet = smoothstep(self.wet.value());
        let mix = smoothstep(self.mix24.value());
        if wet == 0.0 {
            return 1.0;
        }
        if wet == 1.0 && mix == 0.0 {
            return self.c12.magnitude_sq(phi);
        }
        if wet == 1.0 && mix == 1.0 {
            return self.c24a.magnitude_sq(phi) * self.c24b.magnitude_sq(phi);
        }
        // Crossfade in progress: |(1 − wet) + wet·((1 − mix)·H12 + mix·H24a·H24b)|².
        let (sin_w, cos_w) = w.sin_cos();
        let (sin_2w, cos_2w) = (2.0 * w).sin_cos();
        let h12 = self.c12.response(cos_w, sin_w, cos_2w, sin_2w);
        let ha = self.c24a.response(cos_w, sin_w, cos_2w, sin_2w);
        let hb = self.c24b.response(cos_w, sin_w, cos_2w, sin_2w);
        let h24 = (ha.0 * hb.0 - ha.1 * hb.1, ha.0 * hb.1 + ha.1 * hb.0);
        let re = (1.0 - wet) + wet * ((1.0 - mix) * h12.0 + mix * h24.0);
        let im = wet * ((1.0 - mix) * h12.1 + mix * h24.1);
        re * re + im * im
    }
}
