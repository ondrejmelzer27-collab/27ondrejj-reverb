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
        let (left, right): (&mut [f32], &mut [f32]) = (left, right);

        self.process_eq(left, right);
        self.process_output_gain(left, right);

        // Lock-free, allocation-free; drops samples if the GUI isn't draining.
        if self.analyzer_tx.push_stereo(left, right) > 0 {
            self.repaint.request_repaint();
        }

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
