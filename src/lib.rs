//! vocal_eq — CLAP + VST3 vocal EQ built on nice-plug.
//!
//! Signal flow: input → [`dsp::EqChain`] (HPF → low shelf → mid peak → high shelf) →
//! output gain → output. The processed output is also pushed (mono) to the
//! [`analyzer_bus`] for the editor's spectrum analyzer.

use std::sync::Arc;

use nice_plug::prelude::*;
use nice_plug_egui::RepaintNotifier;

pub mod analyzer;
pub mod analyzer_bus;
pub mod dsp;
pub mod editor;
pub mod params;

use analyzer_bus::{AnalyzerProducer, AnalyzerReceiver, analyzer_bus};
use dsp::{EqChain, SUB_BLOCK_SIZE};
use params::VocalEqParams;

pub struct VocalEq {
    params: Arc<VocalEqParams>,
    /// Audio-thread EQ. Recreated (allocation-free, plain struct) in `activate()`.
    chain: EqChain,
    analyzer_tx: AnalyzerProducer,
    analyzer_rx: AnalyzerReceiver,
    repaint: RepaintNotifier,
}

impl Default for VocalEq {
    fn default() -> Self {
        let params = Arc::new(VocalEqParams::default());
        let mut chain = EqChain::new(48_000.0);
        chain.set_params_immediate(&params.target_eq_settings());
        // The analyzer ring is allocated here, once, never on the audio thread.
        let (analyzer_tx, analyzer_rx) = analyzer_bus();
        Self {
            params,
            chain,
            analyzer_tx,
            analyzer_rx,
            repaint: RepaintNotifier::new(),
        }
    }
}

impl VocalEq {
    /// The complete audio path for one stereo block (in place). Realtime-safe.
    fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        // Subnormal input would make every f32 operation below (the f32 → f64 conversion,
        // the output gain, the analyzer's mono sum) take a slow path on x86 CPUs, as FTZ/DAZ
        // are not enabled. Zero them first (inaudible: < -758 dBFS), with integer ops only.
        dsp::flush_denormals(left);
        dsp::flush_denormals(right);

        self.process_eq(left, right);
        self.process_output_gain(left, right);

        // Tiny filter tails or gain < 1 can still produce the odd subnormal: never hand one
        // to the host (validators reject them) or to the analyzer.
        dsp::flush_denormals(left);
        dsp::flush_denormals(right);

        // Lock-free, allocation-free; drops samples if the GUI isn't draining.
        if self.analyzer_tx.push_stereo(left, right) > 0 {
            self.repaint.request_repaint();
        }
    }

    /// Run the EQ over one stereo block. While any EQ smoother is active the block is
    /// split into [`SUB_BLOCK_SIZE`] sub-blocks and the chain gets new settings per
    /// sub-block; otherwise the (unchanged) targets are handed over once and the chain
    /// recomputes nothing unless a discrete param (bypass/slope) changed.
    fn process_eq(&mut self, left: &mut [f32], right: &mut [f32]) {
        let num_samples = left.len().min(right.len());
        if self.params.eq_is_smoothing() {
            let mut start = 0;
            while start < num_samples {
                let end = (start + SUB_BLOCK_SIZE).min(num_samples);
                let settings = self.params.next_eq_settings((end - start) as u32);
                self.chain.set_params(&settings);
                self.chain
                    .process_stereo(&mut left[start..end], &mut right[start..end]);
                start = end;
            }
        } else {
            self.chain.set_params(&self.params.target_eq_settings());
            self.chain
                .process_stereo(&mut left[..num_samples], &mut right[..num_samples]);
        }
    }

    /// Apply the smoothed output gain. Unity gain with no smoothing is skipped so the
    /// default state stays bit-exact transparent.
    fn process_output_gain(&self, left: &mut [f32], right: &mut [f32]) {
        let smoother = &self.params.output_gain.smoothed;
        if smoother.is_smoothing() {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let gain = smoother.next();
                *l *= gain;
                *r *= gain;
            }
        } else {
            let gain = smoother.next();
            if gain != 1.0 {
                for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                    *l *= gain;
                    *r *= gain;
                }
            }
        }
    }
}

impl Plugin for VocalEq {
    const NAME: &'static str = "Vocal EQ";
    const VENDOR: &'static str = "vocal_eq";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    type Editor = editor::VocalEqEditor;
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Self::Editor> {
        editor::create(
            self.params.clone(),
            self.analyzer_rx.clone(),
            self.repaint.clone(),
        )
    }

    fn activate(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl ActivateContext<Self>,
    ) -> bool {
        // Not on the audio thread: safe to (re)build state here. The wrapper has already
        // snapped every smoother to its target, so start the chain at the targets too.
        self.chain = EqChain::new(buffer_config.sample_rate as f64);
        self.chain
            .set_params_immediate(&self.params.target_eq_settings());
        self.analyzer_tx.set_sample_rate(buffer_config.sample_rate);
        true
    }

    fn reset(&mut self) {
        // May be called on the audio thread: only clears filter state, no allocation.
        self.chain.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let [left, right] = buffer.as_slice() else {
            return ProcessStatus::Normal;
        };
        self.process_block(left, right);
        ProcessStatus::Normal
    }
}

impl ClapPlugin for VocalEq {
    const CLAP_ID: &'static str = "com.vocal-eq.vocal-eq";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Vocal EQ: high-pass, low shelf, mid peak, high shelf with spectrum analyzer");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Equalizer,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for VocalEq {
    const VST3_CLASS_ID: [u8; 16] = *b"VocalEqPlugin01A";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Eq];
}

nice_export_clap!(VocalEq);
nice_export_vst3!(VocalEq);

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    /// Deterministic noise in [-1, 1) (integer LCG, no FP on subnormals).
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((s >> 8) as f32 / (1u32 << 23) as f32) - 1.0
            })
            .collect()
    }

    fn float_params(p: &VocalEqParams) -> [&FloatParam; 11] {
        [
            &p.hpf.freq,
            &p.low_shelf.freq,
            &p.low_shelf.gain,
            &p.low_shelf.q,
            &p.peak.freq,
            &p.peak.gain,
            &p.peak.q,
            &p.high_shelf.freq,
            &p.high_shelf.gain,
            &p.high_shelf.q,
            &p.output_gain,
        ]
    }

    /// A plugin as the wrapper hands it to `process()`: every smoother snapped to its
    /// parameter's value (nice-plug does this on activation).
    fn activated_plugin() -> VocalEq {
        let plugin = VocalEq::default();
        for param in float_params(&plugin.params) {
            param.smoothed.reset(param.value());
        }
        plugin
    }

    /// Process `left`/`right` in host blocks whose lengths cycle through `sizes`.
    fn run_blocks(plugin: &mut VocalEq, left: &mut [f32], right: &mut [f32], sizes: &[usize]) {
        let rx = plugin.analyzer_rx.clone();
        let mut pos = 0;
        for &n in sizes.iter().cycle() {
            if pos == left.len() {
                break;
            }
            let end = (pos + n).min(left.len());
            plugin.process_block(&mut left[pos..end], &mut right[pos..end]);
            pos = end;
            rx.drain(|_| {});
        }
    }

    #[test]
    fn subnormal_input_is_flushed_and_never_output() {
        let mut plugin = activated_plugin();
        let rx = plugin.analyzer_rx.clone();
        let mut left = noise(1_000, 1);
        let mut right = noise(1_000, 2);
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            if i % 3 == 0 {
                *l = f32::from_bits(1 + i as u32);
                *r = -f32::from_bits(0x7f_0000 + i as u32);
            }
        }
        let (in_l, in_r) = (left.clone(), right.clone());
        plugin.process_block(&mut left, &mut right);
        for (x, y) in in_l.iter().chain(&in_r).zip(left.iter().chain(&right)) {
            if x.is_subnormal() {
                assert_eq!(*y, 0.0);
                assert_eq!(y.is_sign_negative(), x.is_sign_negative());
            } else {
                // Default params are transparent for everything else.
                assert_eq!(x.to_bits(), y.to_bits());
            }
        }
        let mut pushed = 0;
        rx.drain(|s| {
            pushed += s.len();
            assert!(s.iter().all(|v| v.is_finite() && !v.is_subnormal()));
        });
        assert_eq!(pushed, 1_000);
    }

    #[test]
    fn default_params_are_transparent_at_any_block_size() {
        let mut plugin = activated_plugin();
        let in_l = noise(20_000, 3);
        let in_r = noise(20_000, 4);
        let (mut left, mut right) = (in_l.clone(), in_r.clone());
        run_blocks(
            &mut plugin,
            &mut left,
            &mut right,
            &[1, 1, 7, 31, 32, 33, 64, 100, 513, 2_048, 4_096],
        );
        assert!(
            in_l.iter()
                .zip(&left)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        assert!(
            in_r.iter()
                .zip(&right)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
    }

    /// While EQ smoothers move, the block is split into sub-blocks; odd and 1-sample host
    /// blocks must advance the smoothers by exactly the number of samples processed and
    /// give (nearly) the same output as one large block.
    #[test]
    fn smoothing_is_block_size_independent() {
        // Smooth for 40 ms of the 50 ms ramp so the whole run is mid-ramp.
        let len = (0.04 * FS) as usize;
        let start = |plugin: &VocalEq| {
            let p = &plugin.params;
            p.low_shelf.gain.smoothed.set_target(FS, -9.0);
            p.peak.freq.smoothed.set_target(FS, 3_000.0);
            p.peak.gain.smoothed.set_target(FS, 12.0);
            p.peak.q.smoothed.set_target(FS, 4.0);
            p.high_shelf.gain.smoothed.set_target(FS, 6.0);
            p.output_gain.smoothed.set_target(FS, 0.5);
        };
        let in_l = noise(len, 5);
        let in_r = noise(len, 6);

        let mut reference = activated_plugin();
        start(&reference);
        let (mut ref_l, mut ref_r) = (in_l.clone(), in_r.clone());
        run_blocks(&mut reference, &mut ref_l, &mut ref_r, &[len]);

        for sizes in [
            &[1usize][..],
            &[1, 2, 3, 5, 31, 33, 7],
            &[97, 1, 32, 64, 13],
        ] {
            let mut plugin = activated_plugin();
            start(&plugin);
            let (mut l, mut r) = (in_l.clone(), in_r.clone());
            run_blocks(&mut plugin, &mut l, &mut r, sizes);

            assert!(plugin.params.eq_is_smoothing(), "{sizes:?}");
            for (a, b) in float_params(&reference.params)
                .iter()
                .zip(float_params(&plugin.params))
            {
                let (x, y) = (a.smoothed.previous_value(), b.smoothed.previous_value());
                assert!(
                    (x - y).abs() <= 1e-4 * x.abs().max(1.0),
                    "{sizes:?}: {x} vs {y}"
                );
            }
            let max_diff = ref_l
                .iter()
                .zip(&l)
                .chain(ref_r.iter().zip(&r))
                .map(|(u, v)| (u - v).abs())
                .fold(0.0f32, f32::max);
            // Coefficients follow the same trajectory, updated at different sub-block
            // boundaries (< 32 samples apart): a small difference, no divergence.
            assert!(max_diff < 0.05, "{sizes:?}: max diff {max_diff}");
            assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        }
    }
}
