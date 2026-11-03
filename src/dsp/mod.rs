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

/// Sub-block length (samples) for coefficient updates while smoothing.
pub const SUB_BLOCK_SIZE: usize = 32;

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
pub struct EqChain {
    sample_rate: f64,
    settings: EqSettings,
}

impl EqChain {
    /// Create a chain at `sample_rate` with [`EqSettings::default`] applied immediately.
    pub fn new(sample_rate: f64) -> Self {
        let _ = sample_rate;
        unimplemented!()
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
        let _ = settings;
        unimplemented!()
    }

    /// Jump to `settings` with no ramps (activate/reset, GUI curve chain).
    pub fn set_params_immediate(&mut self, settings: &EqSettings) {
        let _ = settings;
        unimplemented!()
    }

    /// Clear all filter state (delay lines) and finish any pending ramps.
    pub fn reset(&mut self) {
        unimplemented!()
    }

    /// Process one stereo block in place. `left.len()` must equal `right.len()`;
    /// any length (including 0 and lengths not divisible by [`SUB_BLOCK_SIZE`]) is valid.
    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let _ = (left, right);
        unimplemented!()
    }

    /// Combined magnitude response in dB at `freq` Hz (all non-bypassed bands).
    pub fn magnitude_db(&self, freq: f64) -> f64 {
        let _ = freq;
        unimplemented!()
    }

    /// Magnitude response in dB of a single band at `freq` Hz (0 dB if bypassed).
    pub fn band_magnitude_db(&self, band: Band, freq: f64) -> f64 {
        let _ = (band, freq);
        unimplemented!()
    }
}
