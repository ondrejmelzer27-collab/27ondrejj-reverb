//! Plugin parameters (shared contract).
//!
//! Every continuous audio parameter has a smoother. EQ params (freq/gain/Q) are advanced
//! per sub-block via [`VocalEqParams::next_eq_settings`]; output gain is advanced per
//! sample by the plugin's `process()`.
//!
//! Parameter IDs are part of saved state — never rename them.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use nice_plug::prelude::*;

use crate::dsp::{BandSettings, EqSettings, HpfSettings, HpfSlope};

/// Smoothing time for all continuous parameters.
pub const SMOOTHING_MS: f32 = 50.0;

/// Gain range of the shelf/peak bands, in dB (symmetric).
pub const BAND_GAIN_RANGE_DB: f32 = 18.0;

/// Output gain range, in dB (symmetric).
pub const OUTPUT_GAIN_RANGE_DB: f32 = 24.0;

/// Default/min editor size in logical pixels.
pub const DEFAULT_EDITOR_SIZE: (f32, f32) = (960.0, 600.0);
pub const MIN_EDITOR_SIZE: (f32, f32) = (640.0, 420.0);

/// HPF slope as a host-visible enum parameter.
#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpfSlopeParam {
    #[id = "12"]
    #[name = "12 dB/oct"]
    Db12,
    #[id = "24"]
    #[name = "24 dB/oct"]
    Db24,
}

impl From<HpfSlopeParam> for HpfSlope {
    fn from(s: HpfSlopeParam) -> Self {
        match s {
            HpfSlopeParam::Db12 => HpfSlope::Db12,
            HpfSlopeParam::Db24 => HpfSlope::Db24,
        }
    }
}

/// High-pass filter parameters.
#[derive(Params)]
pub struct HpfParams {
    /// Cutoff, 20–500 Hz.
    #[id = "freq"]
    pub freq: FloatParam,
    #[id = "slope"]
    pub slope: EnumParam<HpfSlopeParam>,
    /// `true` = bypassed.
    #[id = "bypass"]
    pub bypass: BoolParam,
}

/// Shelf or peak band parameters.
#[derive(Params)]
pub struct BandParams {
    #[id = "freq"]
    pub freq: FloatParam,
    /// Gain in dB, ±18.
    #[id = "gain"]
    pub gain: FloatParam,
    #[id = "q"]
    pub q: FloatParam,
    /// `true` = bypassed.
    #[id = "bypass"]
    pub bypass: BoolParam,
}

#[derive(Params)]
pub struct VocalEqParams {
    /// Logical editor width, persisted with the plugin state (written by the editor on
    /// resize, applied when the editor opens).
    #[persist = "editor-width"]
    pub editor_width: Arc<AtomicF32>,
    /// Logical editor height, persisted with the plugin state.
    #[persist = "editor-height"]
    pub editor_height: Arc<AtomicF32>,
    /// GUI-only: apply the 4.5 dB/oct tilt to the spectrum analyzer display.
    #[persist = "analyzer-tilt"]
    pub analyzer_tilt: Arc<AtomicBool>,

    #[nested(id_prefix = "hpf", group = "High-Pass")]
    pub hpf: HpfParams,
    #[nested(id_prefix = "ls", group = "Low Shelf")]
    pub low_shelf: BandParams,
    #[nested(id_prefix = "pk", group = "Mid Peak")]
    pub peak: BandParams,
    #[nested(id_prefix = "hs", group = "High Shelf")]
    pub high_shelf: BandParams,

    /// Output gain as a *linear* gain factor (displayed in dB), ±24 dB.
    #[id = "out_gain"]
    pub output_gain: FloatParam,
}

fn freq_param(name: String, default: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(
        name,
        default,
        FloatRange::Skewed {
            min,
            max,
            factor: FloatRange::skew_factor(-2.0),
        },
    )
    .with_smoother(SmoothingStyle::Logarithmic(SMOOTHING_MS))
    .with_value_to_string(formatters::v2s_f32_hz_then_khz(0))
    .with_string_to_value(formatters::s2v_f32_hz_then_khz())
}

fn gain_db_param(name: String) -> FloatParam {
    FloatParam::new(
        name,
        0.0,
        FloatRange::Linear {
            min: -BAND_GAIN_RANGE_DB,
            max: BAND_GAIN_RANGE_DB,
        },
    )
    .with_smoother(SmoothingStyle::Linear(SMOOTHING_MS))
    .with_step_size(0.1)
    .with_unit(" dB")
    .with_value_to_string(formatters::v2s_f32_rounded(1))
}

fn q_param(name: String, default: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(
        name,
        default,
        FloatRange::Skewed {
            min,
            max,
            factor: FloatRange::skew_factor(-1.5),
        },
    )
    .with_smoother(SmoothingStyle::Logarithmic(SMOOTHING_MS))
    .with_value_to_string(formatters::v2s_f32_rounded(2))
}

impl BandParams {
    fn new(
        label: &str,
        default_freq: f32,
        (min_freq, max_freq): (f32, f32),
        default_q: f32,
        (min_q, max_q): (f32, f32),
    ) -> Self {
        Self {
            freq: freq_param(format!("{label} Freq"), default_freq, min_freq, max_freq),
            gain: gain_db_param(format!("{label} Gain")),
            q: q_param(format!("{label} Q"), default_q, min_q, max_q),
            bypass: BoolParam::new(format!("{label} Bypass"), false),
        }
    }

    /// Advance the smoothers by `steps` samples and return the resulting settings.
    fn next_settings(&self, steps: u32) -> BandSettings {
        BandSettings {
            freq_hz: self.freq.smoothed.next_step(steps) as f64,
            gain_db: self.gain.smoothed.next_step(steps) as f64,
            q: self.q.smoothed.next_step(steps) as f64,
            bypass: self.bypass.value(),
        }
    }

    /// Unsmoothed target settings.
    fn target_settings(&self) -> BandSettings {
        BandSettings {
            freq_hz: self.freq.value() as f64,
            gain_db: self.gain.value() as f64,
            q: self.q.value() as f64,
            bypass: self.bypass.value(),
        }
    }

    fn is_smoothing(&self) -> bool {
        self.freq.smoothed.is_smoothing()
            || self.gain.smoothed.is_smoothing()
            || self.q.smoothed.is_smoothing()
    }
}

impl Default for VocalEqParams {
    fn default() -> Self {
        let shelf_q = std::f32::consts::FRAC_1_SQRT_2;
        Self {
            editor_width: Arc::new(AtomicF32::new(DEFAULT_EDITOR_SIZE.0)),
            editor_height: Arc::new(AtomicF32::new(DEFAULT_EDITOR_SIZE.1)),
            analyzer_tilt: Arc::new(AtomicBool::new(true)),

            hpf: HpfParams {
                freq: freq_param("HPF Freq".into(), 80.0, 20.0, 500.0),
                slope: EnumParam::new("HPF Slope", HpfSlopeParam::Db12),
                bypass: BoolParam::new("HPF Bypass", true),
            },
            low_shelf: BandParams::new("Low Shelf", 150.0, (20.0, 2_000.0), shelf_q, (0.3, 2.0)),
            peak: BandParams::new("Mid Peak", 1_000.0, (100.0, 12_000.0), 1.0, (0.1, 10.0)),
            high_shelf: BandParams::new(
                "High Shelf",
                10_000.0,
                (1_000.0, 20_000.0),
                shelf_q,
                (0.3, 2.0),
            ),

            output_gain: FloatParam::new(
                "Output Gain",
                util::db_to_gain(0.0),
                FloatRange::Skewed {
                    min: util::db_to_gain(-OUTPUT_GAIN_RANGE_DB),
                    max: util::db_to_gain(OUTPUT_GAIN_RANGE_DB),
                    factor: FloatRange::gain_skew_factor(
                        -OUTPUT_GAIN_RANGE_DB,
                        OUTPUT_GAIN_RANGE_DB,
                    ),
                },
            )
            .with_smoother(SmoothingStyle::Logarithmic(SMOOTHING_MS))
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_gain_to_db(1))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
        }
    }
}

impl VocalEqParams {
    /// `true` while any EQ (freq/gain/Q) smoother is still moving. Output gain is not
    /// included (it is smoothed per sample by `process()`).
    pub fn eq_is_smoothing(&self) -> bool {
        self.hpf.freq.smoothed.is_smoothing()
            || self.low_shelf.is_smoothing()
            || self.peak.is_smoothing()
            || self.high_shelf.is_smoothing()
    }

    /// Audio thread: advance all EQ smoothers by `steps` samples and return the
    /// resulting settings. Realtime-safe (no allocation).
    pub fn next_eq_settings(&self, steps: u32) -> EqSettings {
        EqSettings {
            hpf: HpfSettings {
                freq_hz: self.hpf.freq.smoothed.next_step(steps) as f64,
                slope: self.hpf.slope.value().into(),
                bypass: self.hpf.bypass.value(),
            },
            low_shelf: self.low_shelf.next_settings(steps),
            peak: self.peak.next_settings(steps),
            high_shelf: self.high_shelf.next_settings(steps),
        }
    }

    /// Unsmoothed target settings (for activate/reset snapping and GUI curve drawing).
    pub fn target_eq_settings(&self) -> EqSettings {
        EqSettings {
            hpf: HpfSettings {
                freq_hz: self.hpf.freq.value() as f64,
                slope: self.hpf.slope.value().into(),
                bypass: self.hpf.bypass.value(),
            },
            low_shelf: self.low_shelf.target_settings(),
            peak: self.peak.target_settings(),
            high_shelf: self.high_shelf.target_settings(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_transparent() {
        let p = VocalEqParams::default();
        let s = p.target_eq_settings();
        assert!(s.hpf.bypass);
        assert_eq!(s.low_shelf.gain_db, 0.0);
        assert_eq!(s.peak.gain_db, 0.0);
        assert_eq!(s.high_shelf.gain_db, 0.0);
        assert_eq!(p.output_gain.value(), 1.0);
        assert!(!p.eq_is_smoothing());
    }

    /// The editor size and analyzer tilt are not parameters; they travel in the state's
    /// persisted fields (keys are part of saved state, like param IDs).
    #[test]
    fn persisted_fields_round_trip() {
        use std::sync::atomic::Ordering;

        let saved = VocalEqParams::default();
        saved.editor_width.store(1234.5, Ordering::Relaxed);
        saved.editor_height.store(777.25, Ordering::Relaxed);
        saved.analyzer_tilt.store(false, Ordering::Relaxed);
        let fields = saved.serialize_fields();
        let keys: Vec<&str> = fields.keys().map(String::as_str).collect();
        assert_eq!(keys, ["analyzer-tilt", "editor-height", "editor-width"]);

        let restored = VocalEqParams::default();
        restored.deserialize_fields(&fields);
        assert_eq!(restored.editor_width.load(Ordering::Relaxed), 1234.5);
        assert_eq!(restored.editor_height.load(Ordering::Relaxed), 777.25);
        assert!(!restored.analyzer_tilt.load(Ordering::Relaxed));
        assert_eq!(restored.serialize_fields(), fields);

        // An older state without some of the fields keeps those at their defaults.
        let mut partial = fields.clone();
        partial.remove("editor-width");
        let fresh = VocalEqParams::default();
        fresh.deserialize_fields(&partial);
        assert_eq!(
            fresh.editor_width.load(Ordering::Relaxed),
            DEFAULT_EDITOR_SIZE.0
        );
        assert_eq!(fresh.editor_height.load(Ordering::Relaxed), 777.25);
        assert!(!fresh.analyzer_tilt.load(Ordering::Relaxed));
    }

    #[test]
    fn param_ids_are_unique_and_prefixed() {
        let p = VocalEqParams::default();
        let ids: Vec<String> = p.param_map().into_iter().map(|(id, _, _)| id).collect();
        for expected in [
            "hpf_freq",
            "hpf_slope",
            "hpf_bypass",
            "ls_freq",
            "ls_gain",
            "ls_q",
            "ls_bypass",
            "pk_freq",
            "pk_gain",
            "pk_q",
            "pk_bypass",
            "hs_freq",
            "hs_gain",
            "hs_q",
            "hs_bypass",
            "out_gain",
        ] {
            assert!(
                ids.iter().any(|id| id == expected),
                "missing param id {expected}: {ids:?}"
            );
        }
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate param ids");
    }
}
