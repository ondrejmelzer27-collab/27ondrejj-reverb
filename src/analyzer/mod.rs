//! GUI-thread spectrum analyzer (FFT, smoothing, tilt). Owned by the GUI.
//!
//! Everything here runs on the GUI thread; the audio thread only pushes samples into the
//! lock-free ring in [`crate::analyzer_bus`]. All buffers are allocated once in
//! [`SpectrumAnalyzer::new`]; the per-frame path (drain → FFT → smoothing → column mapping)
//! does not allocate.
//!
//! Pipeline:
//! 1. [`SpectrumAnalyzer::drain_from`] copies every pending sample from the
//!    [`AnalyzerReceiver`] into a preallocated staging buffer and feeds it to
//!    [`SpectrumAnalyzer::push_samples`].
//! 2. Samples go into a FIFO holding the newest [`FFT_SIZE`] samples. Every [`HOP_SIZE`] new
//!    samples (50 % overlap) the FIFO is Hann-windowed and transformed with a
//!    [`FFT_SIZE`]-point real FFT. At most [`MAX_FFTS_PER_UPDATE`] FFTs run per call; when the
//!    GUI is far behind, older samples are skipped so the display shows the newest data.
//! 3. Magnitudes are converted to dBFS, normalized so a full-scale sine (on a bin centre)
//!    reads 0 dBFS. The FFTs of one call are combined by per-bin maximum into the target.
//! 4. [`SpectrumAnalyzer::update`] moves the displayed spectrum towards the target with a
//!    fast attack and a slower exponential release (time constants, so the result does not
//!    depend on the frame rate).
//! 5. [`SpectrumAnalyzer::map_columns`] maps bins onto log-spaced pixel columns
//!    (interpolation where bins are sparser than columns, max-aggregation where denser),
//!    optionally applying a [`TILT_DB_PER_OCTAVE`] tilt pivoted at [`TILT_PIVOT_HZ`].

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::analyzer_bus::{ANALYZER_CAPACITY, AnalyzerReceiver};

/// FFT length in samples.
pub const FFT_SIZE: usize = 4096;
/// Hop between consecutive FFTs (50 % overlap).
pub const HOP_SIZE: usize = FFT_SIZE / 2;
/// Number of real-FFT output bins (DC..=Nyquist).
pub const NUM_BINS: usize = FFT_SIZE / 2 + 1;
/// Upper bound on FFTs computed per [`SpectrumAnalyzer::push_samples`] call.
pub const MAX_FFTS_PER_UPDATE: usize = 4;
/// Internal level floor in dBFS (silence).
pub const FLOOR_DB: f32 = -140.0;
/// Display tilt slope.
pub const TILT_DB_PER_OCTAVE: f32 = 4.5;
/// Frequency at which the tilt is 0 dB.
pub const TILT_PIVOT_HZ: f32 = 1_000.0;
/// Attack time constant of the display smoothing.
pub const ATTACK_TIME_S: f32 = 0.010;
/// Release time constant of the display smoothing (exponential decay in dB).
pub const RELEASE_TIME_S: f32 = 0.300;
/// When no samples arrive for this long (host stopped processing) the target falls to the
/// floor so the display decays instead of freezing.
pub const STALE_AFTER_S: f32 = 0.25;
/// Maximum number of display columns supported by [`SpectrumAnalyzer::map_columns`].
pub const MAX_COLUMNS: usize = 2048;

/// Longest input slice that is fully analyzed by one `push_samples` call; anything older is
/// skipped.
const MAX_SAMPLES_PER_UPDATE: usize = FFT_SIZE + (MAX_FFTS_PER_UPDATE - 1) * HOP_SIZE;
const FIFO_MASK: usize = FFT_SIZE - 1;
const _: () = assert!(FFT_SIZE.is_power_of_two());

/// Tilt in dB at `freq_hz`: `+TILT_DB_PER_OCTAVE` per octave above [`TILT_PIVOT_HZ`].
pub fn tilt_db(freq_hz: f32) -> f32 {
    TILT_DB_PER_OCTAVE * (freq_hz.max(1.0) / TILT_PIVOT_HZ).log2()
}

/// How one display column is derived from the FFT bins.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ColumnSource {
    /// Column lies above Nyquist.
    Empty,
    /// Fewer than one bin per column: linear interpolation between `bin` and `bin + 1`.
    Interp { bin: usize, frac: f32 },
    /// Several bins per column: maximum over `start..=end`.
    Max { start: usize, end: usize },
}

/// Cache key for the column map.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ColumnKey {
    columns: usize,
    f_min: f32,
    f_max: f32,
}

/// GUI-thread FFT spectrum analyzer. See the module docs for the pipeline.
pub struct SpectrumAnalyzer {
    sample_rate: f32,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// Power normalization for bins `1..NUM_BINS-1` (one-sided spectrum, factor 2).
    power_scale: f32,
    /// Power normalization for DC and Nyquist.
    power_scale_edge: f32,

    /// Circular FIFO with the newest `FFT_SIZE` samples; `write_pos` is the oldest sample.
    fifo: Vec<f32>,
    write_pos: usize,
    /// Valid samples in the FIFO (saturates at `FFT_SIZE`).
    filled: usize,
    /// Samples pushed since the last FFT.
    since_last_fft: usize,

    fft_input: Vec<f32>,
    fft_output: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,

    /// Target spectrum in dBFS (max of the FFTs of the latest update).
    target_db: Vec<f32>,
    /// Displayed (smoothed) spectrum in dBFS.
    smoothed_db: Vec<f32>,
    /// Per-bin tilt in dB.
    tilt_table: Vec<f32>,

    staging: Vec<f32>,
    columns: Vec<ColumnSource>,
    column_key: Option<ColumnKey>,

    idle_secs: f32,
    total_ffts: u64,
}

impl SpectrumAnalyzer {
    /// Allocate all buffers and plan the FFT. Call outside the audio thread.
    pub fn new(sample_rate: f32) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        // Periodic Hann window.
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let phase = std::f64::consts::TAU * i as f64 / FFT_SIZE as f64;
                (0.5 - 0.5 * phase.cos()) as f32
            })
            .collect();
        let window_sum: f32 = window.iter().sum();
        let amp_scale = 2.0 / window_sum;
        let amp_scale_edge = 1.0 / window_sum;

        let mut analyzer = Self {
            sample_rate: sanitize_sample_rate(sample_rate),
            fft_input: fft.make_input_vec(),
            fft_output: fft.make_output_vec(),
            fft_scratch: fft.make_scratch_vec(),
            fft,
            window,
            power_scale: amp_scale * amp_scale,
            power_scale_edge: amp_scale_edge * amp_scale_edge,
            fifo: vec![0.0; FFT_SIZE],
            write_pos: 0,
            filled: 0,
            since_last_fft: 0,
            target_db: vec![FLOOR_DB; NUM_BINS],
            smoothed_db: vec![FLOOR_DB; NUM_BINS],
            tilt_table: vec![0.0; NUM_BINS],
            staging: Vec::with_capacity(ANALYZER_CAPACITY),
            columns: Vec::with_capacity(MAX_COLUMNS),
            column_key: None,
            idle_secs: 0.0,
            total_ffts: 0,
        };
        analyzer.rebuild_tilt_table();
        analyzer
    }

    /// Sample rate the bins are interpreted with.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Width of one FFT bin in Hz.
    pub fn bin_hz(&self) -> f32 {
        self.sample_rate / FFT_SIZE as f32
    }

    /// Total number of FFTs computed since construction.
    pub fn total_ffts(&self) -> u64 {
        self.total_ffts
    }

    /// Displayed (smoothed) spectrum, one dBFS value per bin, without tilt.
    pub fn smoothed_db(&self) -> &[f32] {
        &self.smoothed_db
    }

    /// Change the sample rate. Resets the analyzer when it actually changes; invalid rates
    /// are ignored.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        if !is_valid_sample_rate(sample_rate) || sample_rate == self.sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.rebuild_tilt_table();
        self.column_key = None;
        self.reset();
    }

    /// Forget all buffered samples and drop the display to the floor.
    pub fn reset(&mut self) {
        self.fifo.fill(0.0);
        self.write_pos = 0;
        self.filled = 0;
        self.since_last_fft = 0;
        self.target_db.fill(FLOOR_DB);
        self.smoothed_db.fill(FLOOR_DB);
        self.idle_secs = 0.0;
    }

    /// Drain every pending sample from `rx` (adopting its sample rate) and analyze it.
    /// Returns the number of FFTs computed.
    pub fn drain_from(&mut self, rx: &AnalyzerReceiver) -> usize {
        self.set_sample_rate(rx.sample_rate());

        let mut staging = std::mem::take(&mut self.staging);
        staging.clear();
        rx.drain(|chunk| append_keep_newest(&mut staging, chunk));
        let ffts = self.push_samples(&staging);
        self.staging = staging;
        ffts
    }

    /// Feed one update's worth of samples (oldest first). Runs an FFT every [`HOP_SIZE`]
    /// samples once the FIFO is full, at most [`MAX_FFTS_PER_UPDATE`] per call. If `samples`
    /// is longer than what those FFTs can cover, the oldest samples are skipped. Returns the
    /// number of FFTs computed.
    pub fn push_samples(&mut self, samples: &[f32]) -> usize {
        if samples.is_empty() {
            return 0;
        }
        self.idle_secs = 0.0;

        let samples = if samples.len() > MAX_SAMPLES_PER_UPDATE {
            // Far behind: restart the hop grid on the newest data.
            self.filled = 0;
            self.since_last_fft = 0;
            &samples[samples.len() - MAX_SAMPLES_PER_UPDATE..]
        } else {
            samples
        };

        let mut ffts = 0;
        for &sample in samples {
            self.fifo[self.write_pos] = if sample.is_finite() { sample } else { 0.0 };
            self.write_pos = (self.write_pos + 1) & FIFO_MASK;
            self.filled = (self.filled + 1).min(FFT_SIZE);
            self.since_last_fft += 1;

            if self.filled == FFT_SIZE
                && self.since_last_fft >= HOP_SIZE
                && ffts < MAX_FFTS_PER_UPDATE
            {
                self.run_fft(ffts == 0);
                self.since_last_fft = 0;
                ffts += 1;
            }
        }
        self.total_ffts += ffts as u64;
        ffts
    }

    /// Advance the display smoothing by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        let dt = if dt.is_finite() {
            dt.clamp(0.0, 1.0)
        } else {
            0.0
        };
        // `idle_secs` is reset by every push, so this only triggers after whole frames
        // without any new samples.
        if self.idle_secs > STALE_AFTER_S {
            self.target_db.fill(FLOOR_DB);
        }
        self.idle_secs += dt;

        let attack = 1.0 - (-dt / ATTACK_TIME_S).exp();
        let release = 1.0 - (-dt / RELEASE_TIME_S).exp();
        for (shown, &target) in self.smoothed_db.iter_mut().zip(&self.target_db) {
            let coeff = if target > *shown { attack } else { release };
            *shown += (target - *shown) * coeff;
        }
    }

    /// Smoothed level in dBFS at `freq_hz` (linear interpolation between bins), optionally
    /// with the display tilt applied.
    pub fn level_db_at(&self, freq_hz: f32, tilt: bool) -> f32 {
        let pos = (freq_hz / self.bin_hz()).max(0.0);
        if pos > (NUM_BINS - 1) as f32 {
            return FLOOR_DB;
        }
        let bin = (pos as usize).min(NUM_BINS - 2);
        let frac = pos - bin as f32;
        let level = self.smoothed_db[bin] * (1.0 - frac) + self.smoothed_db[bin + 1] * frac;
        if tilt {
            level + tilt_db(freq_hz)
        } else {
            level
        }
    }

    /// Frequency and level of the loudest smoothed bin (excluding DC).
    pub fn peak(&self) -> (f32, f32) {
        let (bin, level) = self.smoothed_db[1..].iter().enumerate().fold(
            (0, f32::NEG_INFINITY),
            |best, (i, &db)| {
                if db > best.1 { (i + 1, db) } else { best }
            },
        );
        (bin as f32 * self.bin_hz(), level)
    }

    /// Fill `out` with one dBFS value per log-spaced column between `f_min` and `f_max`
    /// (column `c` covers `[c, c + 1] / out.len()` of the log axis). Uses at most
    /// [`MAX_COLUMNS`] columns; any extra entries are set to [`FLOOR_DB`].
    pub fn map_columns(&mut self, f_min: f32, f_max: f32, tilt: bool, out: &mut [f32]) {
        let columns = out.len().min(MAX_COLUMNS);
        self.ensure_column_map(columns, f_min, f_max);

        let value = |bin: usize| -> f32 {
            let db = self.smoothed_db[bin];
            if tilt { db + self.tilt_table[bin] } else { db }
        };
        for (slot, source) in out.iter_mut().zip(&self.columns) {
            *slot = match *source {
                ColumnSource::Empty => FLOOR_DB,
                ColumnSource::Interp { bin, frac } => {
                    value(bin) * (1.0 - frac) + value(bin + 1) * frac
                }
                ColumnSource::Max { start, end } => {
                    (start..=end).map(value).fold(f32::NEG_INFINITY, f32::max)
                }
            };
        }
        for slot in out.iter_mut().skip(columns) {
            *slot = FLOOR_DB;
        }
    }

    fn ensure_column_map(&mut self, columns: usize, f_min: f32, f_max: f32) {
        let key = ColumnKey {
            columns,
            f_min,
            f_max,
        };
        if self.column_key == Some(key) {
            return;
        }
        self.column_key = Some(key);
        self.columns.clear();

        let bin_hz = self.bin_hz();
        let f_min = f_min.max(1.0);
        let ratio = (f_max / f_min).max(1.0 + 1e-6);
        let last_bin = (NUM_BINS - 1) as f32;
        let freq_at = |t: f32| f_min * ratio.powf(t);
        for c in 0..columns {
            let t0 = c as f32 / columns as f32;
            let t1 = (c + 1) as f32 / columns as f32;
            let b0 = freq_at(t0) / bin_hz;
            let b1 = freq_at(t1) / bin_hz;
            let centre = freq_at(0.5 * (t0 + t1)) / bin_hz;

            let source = if b0 > last_bin {
                ColumnSource::Empty
            } else if b1 - b0 <= 1.0 {
                let pos = centre.min(last_bin);
                let bin = (pos as usize).min(NUM_BINS - 2);
                ColumnSource::Interp {
                    bin,
                    frac: pos - bin as f32,
                }
            } else {
                let start = (b0.ceil() as usize).min(NUM_BINS - 1);
                let end = ((b1.ceil() as usize).saturating_sub(1)).min(NUM_BINS - 1);
                if end >= start {
                    ColumnSource::Max { start, end }
                } else {
                    ColumnSource::Interp {
                        bin: start.min(NUM_BINS - 2),
                        frac: 0.0,
                    }
                }
            };
            self.columns.push(source);
        }
    }

    fn rebuild_tilt_table(&mut self) {
        let bin_hz = self.bin_hz();
        for (bin, slot) in self.tilt_table.iter_mut().enumerate() {
            // DC has no meaningful frequency; reuse bin 1's tilt.
            *slot = tilt_db(bin.max(1) as f32 * bin_hz);
        }
    }

    /// Window the FIFO, run the FFT and fold the result into `target_db`.
    fn run_fft(&mut self, first_of_update: bool) {
        let (newer, older) = self.fifo.split_at(self.write_pos);
        let ordered = older.iter().chain(newer);
        for ((dst, &sample), &w) in self.fft_input.iter_mut().zip(ordered).zip(&self.window) {
            *dst = sample * w;
        }
        if self
            .fft
            .process_with_scratch(
                &mut self.fft_input,
                &mut self.fft_output,
                &mut self.fft_scratch,
            )
            .is_err()
        {
            return;
        }

        let last = NUM_BINS - 1;
        for (bin, (target, value)) in self.target_db.iter_mut().zip(&self.fft_output).enumerate() {
            let scale = if bin == 0 || bin == last {
                self.power_scale_edge
            } else {
                self.power_scale
            };
            let power = value.norm_sqr() * scale;
            let db = if power > 1e-30 {
                (10.0 * power.log10()).max(FLOOR_DB)
            } else {
                FLOOR_DB
            };
            *target = if first_of_update { db } else { target.max(db) };
        }
    }
}

fn is_valid_sample_rate(sample_rate: f32) -> bool {
    sample_rate.is_finite() && sample_rate >= 1_000.0
}

fn sanitize_sample_rate(sample_rate: f32) -> f32 {
    if is_valid_sample_rate(sample_rate) {
        sample_rate
    } else {
        48_000.0
    }
}

/// Append `chunk` to `buf` without growing it past its capacity, dropping the oldest samples
/// if necessary. Never allocates.
fn append_keep_newest(buf: &mut Vec<f32>, chunk: &[f32]) {
    let capacity = buf.capacity();
    if chunk.len() >= capacity {
        buf.clear();
        buf.extend_from_slice(&chunk[chunk.len() - capacity..]);
        return;
    }
    let overflow = (buf.len() + chunk.len()).saturating_sub(capacity);
    if overflow > 0 {
        buf.drain(..overflow);
    }
    buf.extend_from_slice(chunk);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn sine(freq: f32, amplitude: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| amplitude * (std::f32::consts::TAU * freq * n as f32 / SR).sin())
            .collect()
    }

    /// Feed in small blocks, like a host would, so the per-call FFT cap never kicks in.
    fn feed_in_blocks(analyzer: &mut SpectrumAnalyzer, samples: &[f32], block: usize) -> usize {
        samples
            .chunks(block)
            .map(|chunk| analyzer.push_samples(chunk))
            .sum()
    }

    #[test]
    fn full_scale_sine_reads_0_dbfs_at_its_frequency() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        feed_in_blocks(&mut analyzer, &sine(1_000.0, 1.0, 3 * FFT_SIZE), 512);
        analyzer.update(1.0);

        let (freq, level) = analyzer.peak();
        assert!(
            (freq - 1_000.0).abs() <= analyzer.bin_hz(),
            "peak at {freq} Hz"
        );
        assert!(level.abs() <= 1.0, "peak level {level} dBFS");

        // Far away from the tone the Hann sidelobes are tiny.
        assert!(analyzer.level_db_at(5_000.0, false) < -80.0);
    }

    #[test]
    fn half_scale_sine_reads_minus_6_dbfs() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        // 3 kHz = 256 * (48 kHz / 4096) is exactly on a bin centre: no scalloping loss.
        feed_in_blocks(&mut analyzer, &sine(3_000.0, 0.5, 2 * FFT_SIZE), 1024);
        analyzer.update(1.0);
        let (_, level) = analyzer.peak();
        assert!((level + 6.02).abs() < 0.05, "level {level}");
    }

    #[test]
    fn silence_reads_floor() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        let ffts = feed_in_blocks(&mut analyzer, &vec![0.0; 3 * FFT_SIZE], 256);
        assert!(ffts > 0);
        analyzer.update(1.0);
        assert!(
            analyzer
                .smoothed_db()
                .iter()
                .all(|&db| db <= FLOOR_DB + 1e-3)
        );

        let mut columns = vec![0.0; 300];
        analyzer.map_columns(20.0, 20_000.0, true, &mut columns);
        assert!(columns.iter().all(|&db| db < -96.0), "{columns:?}");
    }

    #[test]
    fn tilt_is_4_5_db_per_octave_around_1_khz() {
        assert_eq!(tilt_db(TILT_PIVOT_HZ), 0.0);
        assert!((tilt_db(2_000.0) - tilt_db(1_000.0) - 4.5).abs() < 1e-4);
        assert!((tilt_db(500.0) + 4.5).abs() < 1e-4);

        // Two equal tones an octave apart: with tilt the upper one reads ~4.5 dB higher.
        let mut analyzer = SpectrumAnalyzer::new(SR);
        let low = sine(1_000.0, 0.25, 3 * FFT_SIZE);
        let high = sine(2_000.0, 0.25, 3 * FFT_SIZE);
        let mix: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        feed_in_blocks(&mut analyzer, &mix, 512);
        analyzer.update(1.0);

        let bin_level = |freq: f32, tilt: bool| {
            let bin = (freq / analyzer.bin_hz()).round();
            analyzer.level_db_at(bin * analyzer.bin_hz(), tilt)
        };
        let flat = bin_level(2_000.0, false) - bin_level(1_000.0, false);
        let tilted = bin_level(2_000.0, true) - bin_level(1_000.0, true);
        assert!(flat.abs() < 0.2, "untilted difference {flat}");
        assert!((tilted - 4.5).abs() < 0.2, "tilted difference {tilted}");
    }

    #[test]
    fn column_map_applies_tilt() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        analyzer.smoothed_db.fill(-40.0);

        // 30 columns per decade-ish: find the columns containing 1 kHz and 2 kHz.
        let columns = 600;
        let mut out = vec![0.0; columns];
        analyzer.map_columns(20.0, 20_000.0, true, &mut out);
        let column_of = |f: f32| {
            let t = (f / 20.0).ln() / 1000.0f32.ln();
            ((t * columns as f32) as usize).min(columns - 1)
        };
        let diff = out[column_of(2_000.0)] - out[column_of(1_000.0)];
        assert!((diff - 4.5).abs() < 0.15, "diff {diff}");

        analyzer.map_columns(20.0, 20_000.0, false, &mut out);
        assert!(out.iter().all(|&db| (db + 40.0).abs() < 1e-4));
    }

    #[test]
    fn smoothing_decays_over_time_independent_of_frame_rate() {
        let tone = sine(1_000.0, 1.0, 3 * FFT_SIZE);
        let mut a = SpectrumAnalyzer::new(SR);
        let mut b = SpectrumAnalyzer::new(SR);
        for analyzer in [&mut a, &mut b] {
            feed_in_blocks(analyzer, &tone, 512);
            analyzer.update(1.0);
            feed_in_blocks(analyzer, &vec![0.0; 2 * FFT_SIZE], 512);
        }
        let start = a.level_db_at(1_000.0, false);
        assert!(start > -2.0);

        // Release is gradual, not instant...
        let mut previous = start;
        for _ in 0..10 {
            a.update(0.01);
            let now = a.level_db_at(1_000.0, false);
            assert!(
                now < previous,
                "level must keep falling: {now} >= {previous}"
            );
            previous = now;
        }
        assert!(previous > -60.0, "release too fast: {previous}");

        // ...and frame-rate independent.
        b.update(0.1);
        assert!((b.level_db_at(1_000.0, false) - previous).abs() < 1e-2);

        // Eventually it reaches the floor.
        for _ in 0..200 {
            a.update(1.0 / 60.0);
        }
        assert!(a.level_db_at(1_000.0, false) < -130.0);
    }

    #[test]
    fn attack_is_fast() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        feed_in_blocks(&mut analyzer, &sine(1_000.0, 1.0, FFT_SIZE), 512);
        analyzer.update(1.0 / 60.0);
        let (_, level) = analyzer.peak();
        assert!(level > -30.0, "level after one frame {level}");
    }

    #[test]
    fn stale_input_decays_to_floor() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        feed_in_blocks(&mut analyzer, &sine(1_000.0, 1.0, 2 * FFT_SIZE), 512);
        analyzer.update(0.1);
        assert!(analyzer.peak().1 > -2.0);
        // No more samples at all (host stopped processing).
        for _ in 0..300 {
            analyzer.update(1.0 / 60.0);
        }
        assert!(analyzer.peak().1 < -120.0);
    }

    #[test]
    fn hop_count_matches_50_percent_overlap() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        assert_eq!(
            feed_in_blocks(&mut analyzer, &vec![0.1; FFT_SIZE - 1], 100),
            0
        );
        assert_eq!(analyzer.push_samples(&[0.1]), 1);
        assert_eq!(
            feed_in_blocks(&mut analyzer, &vec![0.1; HOP_SIZE - 1], 100),
            0
        );
        assert_eq!(analyzer.push_samples(&[0.1]), 1);

        for n in [
            0,
            1,
            FFT_SIZE,
            FFT_SIZE + HOP_SIZE - 1,
            FFT_SIZE + 7 * HOP_SIZE + 5,
        ] {
            let mut analyzer = SpectrumAnalyzer::new(SR);
            let ffts = feed_in_blocks(&mut analyzer, &vec![0.1; n.max(1)][..n], 333);
            let expected = if n >= FFT_SIZE {
                1 + (n - FFT_SIZE) / HOP_SIZE
            } else {
                0
            };
            assert_eq!(ffts, expected, "n = {n}");
            assert_eq!(analyzer.total_ffts(), expected as u64);
        }
    }

    #[test]
    fn far_behind_skips_to_newest_data_with_capped_ffts() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        // Old loud tone followed by a lot of newer data containing a different tone.
        let mut samples = sine(300.0, 1.0, 20_000);
        samples.extend(sine(4_000.0, 1.0, 12_000));
        let ffts = analyzer.push_samples(&samples);
        assert!((1..=MAX_FFTS_PER_UPDATE).contains(&ffts), "ffts = {ffts}");
        analyzer.update(1.0);

        let (freq, level) = analyzer.peak();
        assert!((freq - 4_000.0).abs() <= analyzer.bin_hz(), "peak {freq}");
        assert!(level.abs() < 1.5);
        assert!(analyzer.level_db_at(300.0, false) < -60.0);
    }

    #[test]
    fn sample_rate_change_resets() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        feed_in_blocks(&mut analyzer, &sine(1_000.0, 1.0, 2 * FFT_SIZE), 512);
        analyzer.update(1.0);
        analyzer.set_sample_rate(96_000.0);
        assert_eq!(analyzer.sample_rate(), 96_000.0);
        assert!((analyzer.bin_hz() - 96_000.0 / FFT_SIZE as f32).abs() < 1e-3);
        assert!(analyzer.peak().1 <= FLOOR_DB);
        // Invalid rates are ignored.
        analyzer.set_sample_rate(f32::NAN);
        analyzer.set_sample_rate(0.0);
        assert_eq!(analyzer.sample_rate(), 96_000.0);
    }

    #[test]
    fn columns_above_nyquist_are_empty() {
        let mut analyzer = SpectrumAnalyzer::new(22_050.0);
        analyzer.smoothed_db.fill(-10.0);
        let mut out = vec![0.0; 200];
        analyzer.map_columns(20.0, 20_000.0, false, &mut out);
        assert_eq!(out[0], -10.0);
        assert_eq!(out[199], FLOOR_DB);
    }

    #[test]
    fn non_finite_input_is_ignored() {
        let mut analyzer = SpectrumAnalyzer::new(SR);
        feed_in_blocks(&mut analyzer, &vec![f32::NAN; 2 * FFT_SIZE], 512);
        analyzer.update(1.0);
        assert!(analyzer.smoothed_db().iter().all(|db| db.is_finite()));
    }

    #[test]
    fn append_keep_newest_never_grows() {
        let mut buf = Vec::with_capacity(8);
        let cap = buf.capacity();
        append_keep_newest(&mut buf, &[1.0, 2.0, 3.0]);
        append_keep_newest(&mut buf, &[4.0; 4]);
        assert_eq!(buf.len(), 7.min(cap));
        let long: Vec<f32> = (0..20).map(|i| i as f32).collect();
        append_keep_newest(&mut buf, &long);
        assert_eq!(buf.capacity(), cap);
        assert_eq!(*buf.last().unwrap(), 19.0);
    }

    #[test]
    fn drain_from_receiver() {
        let (mut tx, rx) = crate::analyzer_bus::analyzer_bus();
        tx.set_sample_rate(44_100.0);
        let tone: Vec<f32> = (0..2 * FFT_SIZE)
            .map(|n| (std::f32::consts::TAU * 1_000.0 * n as f32 / 44_100.0).sin())
            .collect();
        assert_eq!(tx.push_stereo(&tone, &tone), tone.len());

        let mut analyzer = SpectrumAnalyzer::new(48_000.0);
        let ffts = analyzer.drain_from(&rx);
        assert_eq!(analyzer.sample_rate(), 44_100.0);
        assert_eq!(ffts, 1 + FFT_SIZE / HOP_SIZE);
        analyzer.update(1.0);
        let (freq, level) = analyzer.peak();
        assert!((freq - 1_000.0).abs() <= analyzer.bin_hz());
        assert!(level.abs() < 1.5);
        assert_eq!(analyzer.drain_from(&rx), 0);
    }
}
