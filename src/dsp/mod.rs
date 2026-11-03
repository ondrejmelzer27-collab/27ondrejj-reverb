//! Pure DSP for vocal_eq. **No nice-plug imports** — plain Rust + `std` only.
//!
//! Signal flow (per channel): HPF (12 or 24 dB/oct) → low shelf → mid peak → high shelf.
//! Output gain is applied by the plugin layer, not here.
//!
//! # Contract (shared with the plugin layer and the GUI — do not change signatures
//! without coordinating)
//!
//! - [`EqChain::new`] allocates nothing on the heap after construction; every other
//!   method is realtime-safe (no allocation, no locks, no I/O, bounded loops).
//! - [`EqChain::set_params`] sets new *targets*. Continuous values (freq/gain/Q) are
//!   already smoothed by the plugin layer, which calls `set_params` once per
//!   [`SUB_BLOCK_SIZE`]-sample sub-block **only while a smoother is active**.
//!   Coefficients are recomputed only when the settings actually changed.
//!   Discrete changes (band bypass, HPF slope) are de-clicked *inside* the chain with a
//!   short internal ramp, processed in [`SUB_BLOCK_SIZE`] sub-blocks.
//! - [`EqChain::set_params_immediate`] jumps to the settings with no ramps (used on
//!   activate/reset and by the GUI's private curve-drawing chain).
//! - [`EqChain::magnitude_db`] / [`EqChain::band_magnitude_db`] evaluate the analytic
//!   response of the *current* coefficients (cheap enough to call ~1000×/frame on the GUI
//!   thread). Bypassed bands contribute 0 dB.

mod band;
pub mod biquad;
#[cfg(test)]
mod tests;

use band::{HpfBand, ShelfKind, ShelfPeakBand, smoothstep};
use biquad::Coeffs;

/// Sub-block length (samples) for coefficient updates while smoothing.
pub const SUB_BLOCK_SIZE: usize = 32;

/// Length of the internal bypass / HPF-slope de-click ramps, in milliseconds. The
/// ramp is rounded to whole [`SUB_BLOCK_SIZE`] sub-blocks (≈14.5–15.5 ms at
/// 22.05–192 kHz).
pub const DECLICK_RAMP_MS: f64 = 15.0;

/// Sample rate used by [`EqChain::new`] when given a non-finite or < 1 kHz rate.
const FALLBACK_SAMPLE_RATE: f64 = 48_000.0;
const MIN_SAMPLE_RATE: f64 = 1_000.0;

/// High-pass slope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HpfSlope {
    /// 2nd order Butterworth, 12 dB/oct.
    #[default]
    Db12,
    /// 4th order Butterworth (two cascaded biquads), 24 dB/oct.
    Db24,
}

/// Identifies one EQ band (for per-band curves and GUI nodes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Band {
    Hpf,
    LowShelf,
    Peak,
    HighShelf,
}

impl Band {
    /// All bands in signal-flow order.
    pub const ALL: [Band; 4] = [Band::Hpf, Band::LowShelf, Band::Peak, Band::HighShelf];
}

/// High-pass filter settings (plain values, already smoothed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HpfSettings {
    /// Cutoff in Hz (param range 20–500).
    pub freq_hz: f64,
    pub slope: HpfSlope,
    /// `true` = band is bypassed (transparent).
    pub bypass: bool,
}

/// Shelf / peak band settings (plain values, already smoothed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandSettings {
    /// Corner/center frequency in Hz.
    pub freq_hz: f64,
    /// Gain in dB (param range ±18).
    pub gain_db: f64,
    /// RBJ Q (for shelves this is the cookbook's shelf Q; 1/√2 ≙ S = 1).
    pub q: f64,
    /// `true` = band is bypassed (transparent).
    pub bypass: bool,
}

/// Complete EQ settings snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqSettings {
    pub hpf: HpfSettings,
    pub low_shelf: BandSettings,
    pub peak: BandSettings,
    pub high_shelf: BandSettings,
}

impl Default for EqSettings {
    /// Flat/transparent: HPF bypassed, all bands at 0 dB.
    fn default() -> Self {
        Self {
            hpf: HpfSettings {
                freq_hz: 80.0,
                slope: HpfSlope::Db12,
                bypass: true,
            },
            low_shelf: BandSettings {
                freq_hz: 150.0,
                gain_db: 0.0,
                q: std::f64::consts::FRAC_1_SQRT_2,
                bypass: false,
            },
            peak: BandSettings {
                freq_hz: 1000.0,
                gain_db: 0.0,
                q: 1.0,
                bypass: false,
            },
            high_shelf: BandSettings {
                freq_hz: 10_000.0,
                gain_db: 0.0,
                q: std::f64::consts::FRAC_1_SQRT_2,
                bypass: false,
            },
        }
    }
}

/// Stereo vocal EQ chain.
///
/// # Implementation notes
///
/// - **Biquads**: RBJ cookbook designs, Transposed Direct Form II, `f64` coefficients
///   and state, `f32` I/O. A sample stays in `f64` through all bands and is rounded to
///   `f32` once at the end.
/// - **De-click ramps** (bypass, HPF slope): a linear 0↔1 ramp per band, advanced once
///   per [`SUB_BLOCK_SIZE`]-sample sub-block over ≈[`DECLICK_RAMP_MS`].
///   - Shelf/peak bypass: the ramp ("amount") scales the band's gain in dB (0 dB =
///     unity). The band is redesigned once per sub-block during the ramp only, and the
///     coefficients are linearly interpolated per sample across each sub-block from
///     the previous design (stable: the biquad stability region is convex), so the
///     ramp has no per-sub-block steps.
///   - HPF bypass and slope: equal-gain crossfades dry ↔ filtered and 12 ↔ 24 dB/oct
///     with per-sample weights (smoothstep-shaped, so the envelope has no kinks). Both
///     slope paths run while the HPF is engaged, so the inactive one is always warm.
///
///   Sub-blocks are counted from the moment a ramp starts (independent of host block
///   boundaries), so any chunking of the same signal and `set_params` sequence gives
///   the same output. With no ramp running a block is processed in one pass and
///   nothing is recomputed.
/// - **Identity fast path**: a band whose effective gain is exactly 0 dB (incl. a
///   finished bypass ramp) and whose state is zero is skipped; a fully bypassed HPF is
///   skipped and its state cleared. If every band is skipped the buffers are not
///   touched at all, so [`EqSettings::default`] is bit-exact transparent. A band that
///   *reaches* 0 dB keeps running its (input-independent, decaying) tail until the
///   state is < 1e-12 (−240 dBFS), then drops it: no step when a gain is smoothed to
///   0 dB, and bit-exact pass-through a few filter time constants later.
/// - **Denormals**: state below 1e-20 is flushed to zero once per segment (see
///   [`biquad::BiquadState::flush_denormals`]).
pub struct EqChain {
    sample_rate: f64,
    settings: EqSettings,
    hpf: HpfBand,
    low_shelf: ShelfPeakBand,
    peak: ShelfPeakBand,
    high_shelf: ShelfPeakBand,
    /// Ramp increment per sub-block (`1 / sub-blocks per ramp`).
    ramp_step: f64,
    /// Samples left in the current de-click sub-block (0 = none open).
    sub_remaining: usize,
}

/// What one segment (a whole static block, or part of a ramp sub-block) runs.
#[derive(Debug, Clone, Copy)]
struct Plan {
    hpf: bool,
    low_shelf: bool,
    peak: bool,
    high_shelf: bool,
    /// Per-sample coefficient interpolation (bypass ramp sub-block) per shelf/peak band.
    lerp: [bool; 3],
    /// Position of the segment's first sample within the ramp sub-block (1-based);
    /// sample `j` is at `pos0 + j`, i.e. a fraction `(pos0 + j) / SUB_BLOCK_SIZE`.
    /// HPF ramps are `start + slope · (pos0 + j)`.
    pos0: f64,
    wet_start: f64,
    wet_slope: f64,
    mix_start: f64,
    mix_slope: f64,
}

impl Plan {
    fn any(&self) -> bool {
        self.hpf || self.low_shelf || self.peak || self.high_shelf
    }
}

impl EqChain {
    /// Create a chain at `sample_rate` with [`EqSettings::default`] applied immediately.
    ///
    /// A non-finite or nonsensical (< 1 kHz) rate is replaced by 48 kHz so the chain is
    /// always well defined; [`EqChain::sample_rate`] reports the rate actually used.
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = if sample_rate.is_finite() && sample_rate >= MIN_SAMPLE_RATE {
            sample_rate
        } else {
            FALLBACK_SAMPLE_RATE
        };
        let settings = EqSettings::default();
        let ramp_samples = DECLICK_RAMP_MS * 1e-3 * sample_rate;
        let ramp_sub_blocks = (ramp_samples / SUB_BLOCK_SIZE as f64).round().max(1.0);
        Self {
            sample_rate,
            settings,
            hpf: HpfBand::new(&settings.hpf, sample_rate),
            low_shelf: ShelfPeakBand::new(ShelfKind::LowShelf, &settings.low_shelf, sample_rate),
            peak: ShelfPeakBand::new(ShelfKind::Peak, &settings.peak, sample_rate),
            high_shelf: ShelfPeakBand::new(ShelfKind::HighShelf, &settings.high_shelf, sample_rate),
            ramp_step: 1.0 / ramp_sub_blocks,
            sub_remaining: 0,
        }
    }

    /// Sample rate this chain was created with.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Current target settings.
    pub fn settings(&self) -> &EqSettings {
        &self.settings
    }

    /// Set new target settings. Realtime-safe. Recomputes coefficients only if the
    /// settings differ from the current ones; bypass/slope changes are ramped.
    pub fn set_params(&mut self, settings: &EqSettings) {
        if *settings == self.settings {
            return;
        }
        let fs = self.sample_rate;
        self.hpf.set(&settings.hpf, fs);
        self.low_shelf.set(&settings.low_shelf, fs);
        self.peak.set(&settings.peak, fs);
        self.high_shelf.set(&settings.high_shelf, fs);
        self.settings = *settings;
    }

    /// Jump to `settings` with no ramps (activate/reset, GUI curve chain).
    pub fn set_params_immediate(&mut self, settings: &EqSettings) {
        self.set_params(settings);
        self.finish_ramps();
    }

    /// Clear all filter state (delay lines) and finish any pending ramps.
    pub fn reset(&mut self) {
        self.hpf.reset_state();
        for band in [&mut self.low_shelf, &mut self.peak, &mut self.high_shelf] {
            band.reset_state();
        }
        self.finish_ramps();
    }

    /// Process one stereo block in place. `left.len()` must equal `right.len()`;
    /// any length (including 0 and lengths not divisible by [`SUB_BLOCK_SIZE`]) is valid.
    /// If the lengths differ, only the first `min(left.len(), right.len())` samples are
    /// processed.
    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let mut pos = 0;
        // Bounded: every iteration consumes at least one sample, or the rest of the block.
        while pos < n {
            if self.sub_remaining == 0 {
                if !self.any_ramp_moving() {
                    // Static: whole remainder in one pass, nothing recomputed.
                    self.process_segment(&mut left[pos..n], &mut right[pos..n], None);
                    return;
                }
                self.advance_ramps();
                self.sub_remaining = SUB_BLOCK_SIZE;
            }
            let len = self.sub_remaining.min(n - pos);
            let offset = SUB_BLOCK_SIZE - self.sub_remaining;
            let end = pos + len;
            self.process_segment(&mut left[pos..end], &mut right[pos..end], Some(offset));
            self.sub_remaining -= len;
            pos = end;
            if self.sub_remaining == 0 && !self.any_ramp_moving() {
                self.settle_ramps();
            }
        }
    }

    /// Combined magnitude response in dB at `freq` Hz (all non-bypassed bands).
    ///
    /// `freq` is clamped to `[0, fs/2]`; the result is floored at
    /// [`biquad::MIN_MAGNITUDE_DB`] so it is always finite.
    pub fn magnitude_db(&self, freq: f64) -> f64 {
        let (phi, w) = self.eval_point(freq);
        biquad::power_to_db(
            self.hpf.magnitude_sq(phi, w)
                * self.low_shelf.magnitude_sq(phi)
                * self.peak.magnitude_sq(phi)
                * self.high_shelf.magnitude_sq(phi),
        )
    }

    /// Magnitude response in dB of a single band at `freq` Hz (0 dB if bypassed).
    pub fn band_magnitude_db(&self, band: Band, freq: f64) -> f64 {
        let (phi, w) = self.eval_point(freq);
        biquad::power_to_db(match band {
            Band::Hpf => self.hpf.magnitude_sq(phi, w),
            Band::LowShelf => self.low_shelf.magnitude_sq(phi),
            Band::Peak => self.peak.magnitude_sq(phi),
            Band::HighShelf => self.high_shelf.magnitude_sq(phi),
        })
    }

    /// `(φ = sin²(ω/2), ω)` for a display frequency clamped to `[0, fs/2]`.
    fn eval_point(&self, freq: f64) -> (f64, f64) {
        let f = if freq.is_nan() {
            0.0
        } else {
            freq.clamp(0.0, 0.5 * self.sample_rate)
        };
        let w = 2.0 * std::f64::consts::PI * f / self.sample_rate;
        let s = (0.5 * w).sin();
        (s * s, w)
    }

    fn any_ramp_moving(&self) -> bool {
        self.hpf.wet.is_moving()
            || self.hpf.mix24.is_moving()
            || self.low_shelf.amount.is_moving()
            || self.peak.amount.is_moving()
            || self.high_shelf.amount.is_moving()
    }

    /// Start a de-click sub-block: step every ramp, redesign ramping shelf/peak bands.
    fn advance_ramps(&mut self) {
        let step = self.ramp_step;
        let fs = self.sample_rate;
        self.hpf.wet.advance(step);
        self.hpf.mix24.advance(step);
        for band in [&mut self.low_shelf, &mut self.peak, &mut self.high_shelf] {
            band.advance_ramp(step, fs);
        }
    }

    /// All ramps reached their targets at the end of a sub-block.
    fn settle_ramps(&mut self) {
        self.hpf.wet.settle();
        self.hpf.mix24.settle();
        for band in [&mut self.low_shelf, &mut self.peak, &mut self.high_shelf] {
            band.amount.settle();
        }
    }

    /// Complete all ramps instantly (redesigning bands whose amount jumped).
    fn finish_ramps(&mut self) {
        let fs = self.sample_rate;
        self.hpf.wet.snap();
        self.hpf.mix24.snap();
        for band in [&mut self.low_shelf, &mut self.peak, &mut self.high_shelf] {
            band.amount.snap();
            band.update_coeffs(fs);
        }
        self.sub_remaining = 0;
    }

    /// Decide which stages run for a segment. `ramp_offset` is the segment's start
    /// position within the current de-click sub-block, `None` when static.
    fn plan(&self, ramp_offset: Option<usize>) -> Plan {
        let (wet, mix) = (&self.hpf.wet, &self.hpf.mix24);
        let (hpf, pos0, wet_start, wet_slope, mix_start, mix_slope) = match ramp_offset {
            Some(offset) => {
                let inv = 1.0 / SUB_BLOCK_SIZE as f64;
                (
                    wet.start() != 0.0 || wet.value() != 0.0,
                    (offset + 1) as f64,
                    wet.start(),
                    (wet.value() - wet.start()) * inv,
                    mix.start(),
                    (mix.value() - mix.start()) * inv,
                )
            }
            None => (wet.value() != 0.0, 0.0, wet.value(), 0.0, mix.value(), 0.0),
        };
        let ramping = ramp_offset.is_some();
        Plan {
            hpf,
            low_shelf: self.low_shelf.needs_processing(),
            peak: self.peak.needs_processing(),
            high_shelf: self.high_shelf.needs_processing(),
            lerp: [
                ramping && self.low_shelf.interpolating(),
                ramping && self.peak.interpolating(),
                ramping && self.high_shelf.interpolating(),
            ],
            pos0,
            wet_start,
            wet_slope,
            mix_start,
            mix_slope,
        }
    }

    fn process_segment(&mut self, left: &mut [f32], right: &mut [f32], ramp: Option<usize>) {
        let plan = self.plan(ramp);
        if plan.any() {
            self.process_channel(0, left, &plan);
            self.process_channel(1, right, &plan);
        }
        // Housekeeping once per segment (not per sample).
        if plan.hpf {
            self.hpf.flush_denormals();
        } else {
            // Bypassed HPF: skipped; it restarts from silence when engaged again.
            self.hpf.reset_state();
        }
        for band in [&mut self.low_shelf, &mut self.peak, &mut self.high_shelf] {
            band.end_segment();
        }
    }

    /// Run one channel through the planned stages. Coefficients and state are copied
    /// into locals for the loop and the state is written back afterwards.
    fn process_channel(&mut self, ch: usize, buf: &mut [f32], p: &Plan) {
        let hpf = &mut self.hpf;
        let (c12, c24a, c24b) = (hpf.c12, hpf.c24a, hpf.c24b);
        let (mut s12, mut s24a, mut s24b) = (hpf.s12[ch], hpf.s24a[ch], hpf.s24b[ch]);
        let (p_ls, c_ls) = (self.low_shelf.prev_coeffs, self.low_shelf.coeffs);
        let (p_pk, c_pk) = (self.peak.prev_coeffs, self.peak.coeffs);
        let (p_hs, c_hs) = (self.high_shelf.prev_coeffs, self.high_shelf.coeffs);
        let mut s_ls = self.low_shelf.state[ch];
        let mut s_pk = self.peak.state[ch];
        let mut s_hs = self.high_shelf.state[ch];
        let inv_sub = 1.0 / SUB_BLOCK_SIZE as f64;

        for (j, sample) in buf.iter_mut().enumerate() {
            let mut y = f64::from(*sample);
            // Position within the ramp sub-block (only meaningful while ramping).
            let pos = p.pos0 + j as f64;
            if p.hpf {
                let wet = smoothstep(p.wet_start + p.wet_slope * pos);
                let mix = smoothstep(p.mix_start + p.mix_slope * pos);
                let y12 = s12.tick(&c12, y);
                let y24 = s24b.tick(&c24b, s24a.tick(&c24a, y));
                // With weights exactly 0/1 these reduce to the plain path (bit-exact).
                let filtered = (1.0 - mix) * y12 + mix * y24;
                y = (1.0 - wet) * y + wet * filtered;
            }
            if p.low_shelf {
                y = if p.lerp[0] {
                    s_ls.tick(&Coeffs::lerp(&p_ls, &c_ls, pos * inv_sub), y)
                } else {
                    s_ls.tick(&c_ls, y)
                };
            }
            if p.peak {
                y = if p.lerp[1] {
                    s_pk.tick(&Coeffs::lerp(&p_pk, &c_pk, pos * inv_sub), y)
                } else {
                    s_pk.tick(&c_pk, y)
                };
            }
            if p.high_shelf {
                y = if p.lerp[2] {
                    s_hs.tick(&Coeffs::lerp(&p_hs, &c_hs, pos * inv_sub), y)
                } else {
                    s_hs.tick(&c_hs, y)
                };
            }
            *sample = y as f32;
        }

        if p.hpf {
            hpf.s12[ch] = s12;
            hpf.s24a[ch] = s24a;
            hpf.s24b[ch] = s24b;
        }
        self.low_shelf.state[ch] = s_ls;
        self.peak.state[ch] = s_pk;
        self.high_shelf.state[ch] = s_hs;
    }
}

/// Test-only introspection.
#[cfg(test)]
impl EqChain {
    /// Total number of band coefficient (re)designs since construction.
    pub(crate) fn coeff_recomputes(&self) -> usize {
        self.hpf.recomputes
            + self.low_shelf.recomputes
            + self.peak.recomputes
            + self.high_shelf.recomputes
    }

    /// A de-click ramp (or the tail of its last sub-block) is in progress.
    pub(crate) fn is_ramping(&self) -> bool {
        self.sub_remaining != 0 || self.any_ramp_moving()
    }

    /// Number of sub-blocks a de-click ramp takes.
    pub(crate) fn ramp_sub_blocks(&self) -> usize {
        (1.0 / self.ramp_step).round() as usize
    }

    /// Which stages currently run (HPF, low shelf, peak, high shelf), static view.
    pub(crate) fn active_stages(&self) -> [bool; 4] {
        let p = self.plan(None);
        [p.hpf, p.low_shelf, p.peak, p.high_shelf]
    }

    /// Every biquad state value of the chain (both channels).
    pub(crate) fn all_state_values(&self) -> Vec<f64> {
        let mut v = Vec::new();
        for st in self
            .hpf
            .s12
            .iter()
            .chain(self.hpf.s24a.iter())
            .chain(self.hpf.s24b.iter())
            .chain(self.low_shelf.state.iter())
            .chain(self.peak.state.iter())
            .chain(self.high_shelf.state.iter())
        {
            let (a, b) = st.values();
            v.push(a);
            v.push(b);
        }
        v
    }

    /// Coefficients of every biquad that currently affects the output.
    pub(crate) fn active_coeffs(&self) -> Vec<biquad::Coeffs> {
        let mut v = Vec::new();
        let [hpf, ls, pk, hs] = self.active_stages();
        if hpf {
            v.extend([self.hpf.c12, self.hpf.c24a, self.hpf.c24b]);
        }
        if ls {
            v.push(self.low_shelf.coeffs);
        }
        if pk {
            v.push(self.peak.coeffs);
        }
        if hs {
            v.push(self.high_shelf.coeffs);
        }
        v
    }
}
