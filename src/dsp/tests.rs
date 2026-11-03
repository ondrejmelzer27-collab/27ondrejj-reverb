//! DSP unit tests (std only; deterministic xorshift RNG).

use std::f64::consts::{FRAC_1_SQRT_2, PI};

use super::biquad::{self, Coeffs};
use super::*;

// ---------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------

/// xorshift64* PRNG (tests only; no external crates).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn log_range(&mut self, lo: f64, hi: f64) -> f64 {
        self.range(lo.ln(), hi.ln()).exp()
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn coin(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    fn noise(&mut self, amp: f64) -> f32 {
        (self.range(-1.0, 1.0) * amp) as f32
    }
}

const TEST_RATES: [f64; 3] = [44_100.0, 48_000.0, 96_000.0];
const ALL_RATES: [f64; 8] = [
    22_050.0, 32_000.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0,
];

fn flat() -> EqSettings {
    EqSettings::default()
}

fn hpf_only(freq_hz: f64, slope: HpfSlope) -> EqSettings {
    let mut s = flat();
    s.hpf = HpfSettings {
        freq_hz,
        slope,
        bypass: false,
    };
    s
}

fn band_mut(s: &mut EqSettings, band: Band) -> &mut BandSettings {
    match band {
        Band::LowShelf => &mut s.low_shelf,
        Band::Peak => &mut s.peak,
        Band::HighShelf => &mut s.high_shelf,
        Band::Hpf => panic!("the HPF has no BandSettings"),
    }
}

fn band_only(band: Band, freq_hz: f64, gain_db: f64, q: f64) -> EqSettings {
    let mut s = flat();
    *band_mut(&mut s, band) = BandSettings {
        freq_hz,
        gain_db,
        q,
        bypass: false,
    };
    s
}

/// Every band engaged with audible settings.
fn all_engaged() -> EqSettings {
    EqSettings {
        hpf: HpfSettings {
            freq_hz: 120.0,
            slope: HpfSlope::Db24,
            bypass: false,
        },
        low_shelf: BandSettings {
            freq_hz: 250.0,
            gain_db: 6.0,
            q: FRAC_1_SQRT_2,
            bypass: false,
        },
        peak: BandSettings {
            freq_hz: 1_000.0,
            gain_db: 12.0,
            q: 4.0,
            bypass: false,
        },
        high_shelf: BandSettings {
            freq_hz: 8_000.0,
            gain_db: -9.0,
            q: FRAC_1_SQRT_2,
            bypass: false,
        },
    }
}

fn random_settings(rng: &mut Rng) -> EqSettings {
    let slope = if rng.coin() {
        HpfSlope::Db12
    } else {
        HpfSlope::Db24
    };
    let gain = |rng: &mut Rng| match rng.below(8) {
        0 => 18.0,
        1 => -18.0,
        2 => 0.0,
        _ => rng.range(-18.0, 18.0),
    };
    EqSettings {
        hpf: HpfSettings {
            freq_hz: rng.log_range(20.0, 500.0),
            slope,
            bypass: rng.below(3) == 0,
        },
        low_shelf: BandSettings {
            freq_hz: rng.log_range(20.0, 2_000.0),
            gain_db: gain(rng),
            q: rng.log_range(0.3, 2.0),
            bypass: rng.below(4) == 0,
        },
        peak: BandSettings {
            freq_hz: rng.log_range(100.0, 12_000.0),
            gain_db: gain(rng),
            q: rng.log_range(0.1, 10.0),
            bypass: rng.below(4) == 0,
        },
        high_shelf: BandSettings {
            freq_hz: rng.log_range(1_000.0, 20_000.0),
            gain_db: gain(rng),
            q: rng.log_range(0.3, 2.0),
            bypass: rng.below(4) == 0,
        },
    }
}

fn chain_with(fs: f64, s: &EqSettings) -> EqChain {
    let mut c = EqChain::new(fs);
    c.set_params_immediate(s);
    c
}

/// Largest pole radius of a biquad (roots of z² + a1·z + a2).
fn pole_radius(c: &Coeffs) -> f64 {
    let disc = c.a1 * c.a1 - 4.0 * c.a2;
    if disc < 0.0 {
        c.a2.sqrt()
    } else {
        // Numerically stable real root pair.
        let q = -0.5 * (c.a1 + disc.sqrt().copysign(c.a1));
        let r2 = if q != 0.0 { (c.a2 / q).abs() } else { 0.0 };
        q.abs().max(r2)
    }
}

/// Samples until every active pole has decayed by `decay` (capped at `cap`).
fn settle_len(chain: &EqChain, decay: f64, cap: usize) -> usize {
    let r = chain
        .active_coeffs()
        .iter()
        .map(pole_radius)
        .fold(0.0, f64::max);
    if r <= 0.0 {
        return 0;
    }
    assert!(r < 1.0, "unstable pole radius {r}");
    ((decay.ln() / r.ln()).ceil() as usize).min(cap)
}

fn max_step(v: &[f32]) -> f64 {
    v.windows(2)
        .map(|w| (f64::from(w[1]) - f64::from(w[0])).abs())
        .fold(0.0, f64::max)
}

fn assert_close(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: got {actual}, expected {expected} ± {tol}"
    );
}

/// Periodic multi-tone signal with exact-period DFT projections.
///
/// All tones are multiples of 10 Hz and the sample rate is a multiple of 10, so one
/// period of the signal is `fs / 10` samples and every tone completes a whole number
/// of cycles in it: projecting onto each tone over that window is exactly orthogonal
/// to the others, and phases are computed from exact integer arithmetic.
struct ToneBench {
    period: usize,
    tones: Vec<f64>,
    input: Vec<f32>,
    cos_tab: Vec<Vec<f64>>,
    sin_tab: Vec<Vec<f64>>,
    input_amp: Vec<f64>,
}

impl ToneBench {
    fn new(fs: f64, tones: &[f64], amp: f64) -> Self {
        let fs_i = fs as u64;
        assert!(
            fs_i as f64 == fs && fs_i.is_multiple_of(10),
            "fs must be a multiple of 10"
        );
        let period = (fs_i / 10) as usize;
        let mut cos_tab = Vec::new();
        let mut sin_tab = Vec::new();
        for &f in tones {
            let f_i = f as u64;
            assert!(
                f_i as f64 == f && f_i.is_multiple_of(10) && f < 0.5 * fs,
                "bad tone {f}"
            );
            let phase = |n: usize| 2.0 * PI * ((f_i * n as u64) % fs_i) as f64 / fs_i as f64;
            cos_tab.push((0..period).map(|n| phase(n).cos()).collect::<Vec<_>>());
            sin_tab.push((0..period).map(|n| phase(n).sin()).collect::<Vec<_>>());
        }
        let input = (0..period)
            .map(|n| {
                let x: f64 = (0..tones.len())
                    .map(|k| {
                        // Distinct phase offsets keep the crest factor down.
                        let (s, c) = (0.7 * k as f64).sin_cos();
                        amp * (sin_tab[k][n] * c + cos_tab[k][n] * s)
                    })
                    .sum();
                x as f32
            })
            .collect();
        let mut bench = Self {
            period,
            tones: tones.to_vec(),
            input,
            cos_tab,
            sin_tab,
            input_amp: Vec::new(),
        };
        bench.input_amp = bench.project(&bench.input);
        bench
    }

    /// Amplitude of each tone in `y` (length must be a whole number of periods).
    fn project(&self, y: &[f32]) -> Vec<f64> {
        assert_eq!(y.len() % self.period, 0);
        (0..self.tones.len())
            .map(|k| {
                let (mut re, mut im) = (0.0, 0.0);
                for (n, &v) in y.iter().enumerate() {
                    let i = n % self.period;
                    re += f64::from(v) * self.cos_tab[k][i];
                    im += f64::from(v) * self.sin_tab[k][i];
                }
                2.0 * (re * re + im * im).sqrt() / y.len() as f64
            })
            .collect()
    }

    /// Run the periodic input through `chain` until every pole has decayed by 1e-13,
    /// then measure one period: gain of each tone in dB (left channel; the right
    /// channel gets the same input and must be bit-identical).
    fn measure(&self, chain: &mut EqChain) -> Vec<f64> {
        let cap = 30 * chain.sample_rate() as usize;
        let periods = settle_len(chain, 1e-13, cap).div_ceil(self.period) + 1;
        let mut l = vec![0.0f32; self.period];
        let mut r = vec![0.0f32; self.period];
        for _ in 0..=periods {
            l.copy_from_slice(&self.input);
            r.copy_from_slice(&self.input);
            chain.process_stereo(&mut l, &mut r);
        }
        assert_eq!(l, r, "left and right differ for identical input");
        self.project(&l)
            .iter()
            .zip(&self.input_amp)
            .map(|(o, i)| 20.0 * (o / i).log10())
            .collect()
    }
}

/// Feed `x_l`/`x_r` through `chain`, applying `events` (sample index, settings) with
/// `set_params`. Each inter-event segment is split into chunks by `chunk(remaining)`.
fn run_events(
    chain: &mut EqChain,
    x_l: &[f32],
    x_r: &[f32],
    events: &[(usize, EqSettings)],
    mut chunk: impl FnMut(usize) -> usize,
) -> (Vec<f32>, Vec<f32>) {
    let len = x_l.len();
    let mut l = x_l.to_vec();
    let mut r = x_r.to_vec();
    let mut pos = 0;
    let mut ev = 0;
    let mut zero_chunks = 0;
    while pos < len {
        while ev < events.len() && events[ev].0 <= pos {
            chain.set_params(&events[ev].1);
            ev += 1;
        }
        let seg_end = events.get(ev).map_or(len, |e| e.0.min(len));
        let mut n = chunk(seg_end - pos).min(seg_end - pos);
        if n == 0 {
            // Zero-length calls are allowed; make sure we still make progress.
            chain.process_stereo(&mut l[pos..pos], &mut r[pos..pos]);
            zero_chunks += 1;
            if zero_chunks < 3 {
                continue;
            }
            n = 1;
        }
        zero_chunks = 0;
        chain.process_stereo(&mut l[pos..pos + n], &mut r[pos..pos + n]);
        pos += n;
    }
    (l, r)
}

fn sine(fs: f64, freq: f64, amp: f64, len: usize) -> Vec<f32> {
    (0..len)
        .map(|n| (amp * (2.0 * PI * freq * n as f64 / fs).sin()) as f32)
        .collect()
}

// ---------------------------------------------------------------------------------
// Frequency response
// ---------------------------------------------------------------------------------

#[test]
fn measured_response_matches_analytic() {
    const TONES: [f64; 7] = [30.0, 100.0, 300.0, 1_000.0, 3_000.0, 10_000.0, 15_000.0];
    const GAINS: [f64; 4] = [-18.0, -6.0, 3.0, 18.0];
    let mut worst = 0.0f64;
    let mut worst_case = String::new();
    let mut checks = 0;
    for &fs in &TEST_RATES {
        let bench = ToneBench::new(fs, &TONES, 0.1);
        let mut configs: Vec<(Band, EqSettings)> = Vec::new();
        for fc in [20.0, 30.0, 100.0, 300.0, 500.0] {
            for slope in [HpfSlope::Db12, HpfSlope::Db24] {
                configs.push((Band::Hpf, hpf_only(fc, slope)));
            }
        }
        let shelf_q = [0.3, FRAC_1_SQRT_2, 2.0];
        for (band, freqs, qs) in [
            (Band::LowShelf, [20.0, 300.0, 2_000.0], shelf_q),
            (Band::Peak, [100.0, 1_000.0, 12_000.0], [0.1, 1.0, 10.0]),
            (Band::HighShelf, [1_000.0, 8_000.0, 20_000.0], shelf_q),
        ] {
            for f0 in freqs {
                for gain in GAINS {
                    for q in qs {
                        configs.push((band, band_only(band, f0, gain, q)));
                    }
                }
            }
        }
        for (band, s) in &configs {
            let mut chain = chain_with(fs, s);
            let measured = bench.measure(&mut chain);
            for (k, &f) in TONES.iter().enumerate() {
                let analytic = chain.band_magnitude_db(*band, f);
                let total = chain.magnitude_db(f);
                assert_close(total, analytic, 1e-9, "chain vs band magnitude");
                let err = (measured[k] - analytic).abs();
                let what = format!("{band:?} {s:?} @ {f} Hz, fs {fs}");
                assert!(
                    err < 0.1,
                    "{what}: measured {} dB, analytic {analytic} dB",
                    measured[k]
                );
                if err > worst {
                    worst = err;
                    worst_case = format!("{what}: analytic {analytic:.3} dB");
                }
                checks += 1;
            }
        }
    }
    println!("measured vs analytic: {checks} checks, worst error {worst:.2e} dB ({worst_case})");
    assert!(worst < 0.02, "worst error {worst} dB");
}

#[test]
fn sanity_anchors_analytic() {
    for &fs in &TEST_RATES {
        // Peak: exactly gain_db at the center frequency.
        for f0 in [100.0, 1_000.0, 5_000.0, 12_000.0] {
            for g in [-18.0, -6.0, 6.0, 18.0] {
                for q in [0.1, 1.0, 10.0] {
                    let c = chain_with(fs, &band_only(Band::Peak, f0, g, q));
                    assert_close(c.band_magnitude_db(Band::Peak, f0), g, 1e-6, "peak center");
                }
            }
        }
        // HPF: −3.01 dB at the cutoff (|H(f0)| = Q, Q1·Q2 = 1/√2 for 24 dB/oct).
        for fc in [20.0, 80.0, 500.0] {
            for slope in [HpfSlope::Db12, HpfSlope::Db24] {
                let c = chain_with(fs, &hpf_only(fc, slope));
                assert_close(
                    c.band_magnitude_db(Band::Hpf, fc),
                    -3.0103,
                    1e-3,
                    "HPF cutoff",
                );
            }
        }
        // HPF stopband slope: 12.04 / 24.08 dB per octave (20·log10(2^n)).
        for (slope, per_octave) in [(HpfSlope::Db12, 12.0), (HpfSlope::Db24, 24.0)] {
            let c = chain_with(fs, &hpf_only(500.0, slope));
            let d = c.band_magnitude_db(Band::Hpf, 40.0) - c.band_magnitude_db(Band::Hpf, 20.0);
            assert_close(d, per_octave, 0.1, "HPF slope per octave");
        }
        // Shelves: exact at DC / Nyquist / corner, ≈ gain or 0 dB a factor 100 away.
        let nyq = 0.5 * fs;
        for g in [-18.0, -6.0, 6.0, 18.0] {
            for q in [0.3, FRAC_1_SQRT_2, 2.0] {
                let ls = chain_with(fs, &band_only(Band::LowShelf, 100.0, g, q));
                let m = |f| ls.band_magnitude_db(Band::LowShelf, f);
                assert_close(m(0.0), g, 1e-6, "low shelf DC");
                assert_close(m(nyq), 0.0, 1e-6, "low shelf Nyquist");
                assert_close(m(100.0), 0.5 * g, 1e-6, "low shelf corner");
                assert_close(m(1.0), g, 0.1, "low shelf well inside");
                assert_close(m(10_000.0), 0.0, 0.1, "low shelf far outside");

                let hs = chain_with(fs, &band_only(Band::HighShelf, 200.0, g, q));
                let m = |f| hs.band_magnitude_db(Band::HighShelf, f);
                assert_close(m(0.0), 0.0, 1e-6, "high shelf DC");
                assert_close(m(nyq), g, 1e-6, "high shelf Nyquist");
                assert_close(m(200.0), 0.5 * g, 1e-6, "high shelf corner");
                assert_close(m(20_000.0), g, 0.1, "high shelf well inside");
                assert_close(m(2.0), 0.0, 0.1, "high shelf far outside");
            }
        }
    }
}

#[test]
fn sanity_anchors_measured() {
    for &fs in &TEST_RATES {
        // Peak center = gain.
        let bench = ToneBench::new(fs, &[1_000.0], 0.5);
        for g in [-18.0, -6.0, 6.0, 18.0] {
            for q in [0.1, 1.0, 10.0] {
                let mut c = chain_with(fs, &band_only(Band::Peak, 1_000.0, g, q));
                assert_close(bench.measure(&mut c)[0], g, 0.02, "measured peak center");
            }
        }
        // HPF at cutoff = −3.01 dB, stopband 12 / 24 dB per octave.
        let bench = ToneBench::new(fs, &[20.0, 40.0, 100.0], 0.3);
        for (slope, per_octave) in [(HpfSlope::Db12, 12.0), (HpfSlope::Db24, 24.0)] {
            let mut c = chain_with(fs, &hpf_only(100.0, slope));
            assert_close(
                bench.measure(&mut c)[2],
                -3.0103,
                0.02,
                "measured HPF cutoff",
            );
            let mut c = chain_with(fs, &hpf_only(500.0, slope));
            let m = bench.measure(&mut c);
            assert_close(m[1] - m[0], per_octave, 0.1, "measured HPF slope");
        }
        // Shelves: ≈ gain well inside, ≈ 0 dB far outside (factor 100).
        let bench = ToneBench::new(fs, &[10.0, 10_000.0, 20_000.0], 0.3);
        for g in [-18.0, 18.0] {
            for q in [0.3, FRAC_1_SQRT_2, 2.0] {
                let mut c = chain_with(fs, &band_only(Band::LowShelf, 1_000.0, g, q));
                assert_close(bench.measure(&mut c)[0], g, 0.1, "low shelf inside");
                let mut c = chain_with(fs, &band_only(Band::LowShelf, 100.0, g, q));
                assert_close(bench.measure(&mut c)[1], 0.0, 0.1, "low shelf outside");
                let mut c = chain_with(fs, &band_only(Band::HighShelf, 200.0, g, q));
                assert_close(bench.measure(&mut c)[2], g, 0.1, "high shelf inside");
                let mut c = chain_with(fs, &band_only(Band::HighShelf, 1_000.0, g, q));
                assert_close(bench.measure(&mut c)[0], 0.0, 0.1, "high shelf outside");
            }
        }
    }
}

#[test]
fn magnitude_is_finite_and_consistent() {
    let mut rng = Rng::new(11);
    let odd_freqs = [
        f64::NAN,
        f64::NEG_INFINITY,
        -100.0,
        0.0,
        1e-9,
        20.0,
        1e9,
        f64::INFINITY,
    ];
    for _ in 0..200 {
        let fs = ALL_RATES[rng.below(ALL_RATES.len())];
        let mut c = chain_with(fs, &random_settings(&mut rng));
        // Start some ramps so the crossfade evaluation path is covered too.
        c.set_params(&random_settings(&mut rng));
        let mut l = vec![0.0f32; 1 + rng.below(400)];
        let mut r = l.clone();
        c.process_stereo(&mut l, &mut r);
        for &f in &odd_freqs {
            assert!(c.magnitude_db(f).is_finite());
            for band in Band::ALL {
                assert!(c.band_magnitude_db(band, f).is_finite());
            }
        }
        for _ in 0..20 {
            let f = rng.log_range(10.0, 0.5 * fs);
            let sum: f64 = Band::ALL.iter().map(|&b| c.band_magnitude_db(b, f)).sum();
            if sum > biquad::MIN_MAGNITUDE_DB + 1.0 {
                assert_close(c.magnitude_db(f), sum, 1e-9, "chain = Σ bands");
            }
        }
    }
}

// ---------------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------------

#[test]
fn random_settings_never_produce_nan_or_inf() {
    let mut rng = Rng::new(1234);
    for iter in 0..300 {
        let fs = ALL_RATES[rng.below(ALL_RATES.len())];
        let mut c = chain_with(fs, &random_settings(&mut rng));
        let len = 3_000;
        let square_period = 2 + rng.below(200);
        let x: Vec<f32> = (0..len)
            .map(|n| match n * 3 / len {
                0 => rng.noise(1.0),
                1 => {
                    if (n / square_period).is_multiple_of(2) {
                        1.0
                    } else {
                        -1.0
                    }
                }
                _ => 1.0,
            })
            .collect();
        // Abrupt random settings changes (bypass/slope ramps and coefficient jumps).
        let mut events = Vec::new();
        let mut pos = 0;
        while pos < len {
            events.push((pos, random_settings(&mut rng)));
            pos += 1 + rng.below(400);
        }
        let x_r: Vec<f32> = x.iter().map(|v| -v).collect();
        let (l, r) = run_events(&mut c, &x, &x_r, &events, |rem| rng.below(rem + 1));
        assert!(
            l.iter().chain(&r).all(|v| v.is_finite()),
            "non-finite output in iteration {iter} at fs {fs}"
        );
        for f in [20.0, 1_000.0, 20_000.0] {
            assert!(c.magnitude_db(f).is_finite());
        }
    }
}

#[test]
fn out_of_range_settings_are_sanitized() {
    let weird = [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -1.0,
        1e-300,
        1e12,
    ];
    for &fs in &[22_050.0, 48_000.0, 192_000.0] {
        for &v in &weird {
            let mut s = all_engaged();
            s.hpf.freq_hz = v;
            s.low_shelf.freq_hz = v;
            s.peak.q = v;
            s.high_shelf.gain_db = v;
            s.low_shelf.q = v;
            s.peak.gain_db = v;
            let mut c = chain_with(fs, &s);
            for co in c.active_coeffs() {
                assert!(pole_radius(&co) < 1.0, "unstable for value {v}");
            }
            let mut l: Vec<f32> = sine(fs, 440.0, 1.0, 4_096);
            let mut r = l.clone();
            c.process_stereo(&mut l, &mut r);
            assert!(l.iter().all(|x| x.is_finite()), "value {v}");
            assert!(c.magnitude_db(1_000.0).is_finite());
        }
    }
    // Nonsensical sample rates fall back to something usable.
    for fs in [0.0, -1.0, f64::NAN, f64::INFINITY, 10.0] {
        let mut c = chain_with(fs, &all_engaged());
        assert!(c.sample_rate() >= 1_000.0 && c.sample_rate().is_finite());
        let mut l = vec![0.5f32; 256];
        let mut r = l.clone();
        c.process_stereo(&mut l, &mut r);
        assert!(l.iter().all(|x| x.is_finite()));
    }
}

#[test]
fn non_finite_input_does_not_poison_the_filters() {
    let mut c = chain_with(48_000.0, &all_engaged());
    let mut l = vec![0.1f32; 512];
    l[100] = f32::NAN;
    l[200] = f32::INFINITY;
    let mut r = l.clone();
    c.process_stereo(&mut l, &mut r);
    // The state is reset at the end of the block; the next block is finite again.
    let mut l = sine(48_000.0, 1_000.0, 0.5, 512);
    let mut r = l.clone();
    c.process_stereo(&mut l, &mut r);
    assert!(l.iter().chain(&r).all(|v| v.is_finite()));
}

#[test]
fn stable_at_extreme_settings() {
    let mut cases: Vec<EqSettings> = Vec::new();
    for slope in [HpfSlope::Db12, HpfSlope::Db24] {
        // 0 Hz and 1 GHz exercise the clamp limits (10 Hz and 0.49·fs).
        for f in [0.0, 20.0, 500.0, 1e9] {
            cases.push(hpf_only(f, slope));
        }
    }
    for (band, freqs, qs) in [
        (Band::LowShelf, [0.0, 20.0, 2_000.0, 1e9], [0.0, 0.3, 2.0]),
        (Band::Peak, [0.0, 100.0, 12_000.0, 1e9], [0.0, 0.1, 10.0]),
        (
            Band::HighShelf,
            [0.0, 1_000.0, 20_000.0, 1e9],
            [0.0, 0.3, 2.0],
        ),
    ] {
        for f in freqs {
            for q in qs {
                for g in [-18.0, 18.0] {
                    cases.push(band_only(band, f, g, q));
                }
            }
        }
    }
    let mut everything = all_engaged();
    everything.peak = BandSettings {
        freq_hz: 12_000.0,
        gain_db: 18.0,
        q: 10.0,
        bypass: false,
    };
    everything.high_shelf.freq_hz = 20_000.0;
    everything.high_shelf.gain_db = 18.0;
    cases.push(everything);

    for &fs in &[22_050.0, 44_100.0, 192_000.0] {
        for s in &cases {
            let mut c = chain_with(fs, s);
            for co in c.active_coeffs() {
                assert!(
                    pole_radius(&co) < 1.0 - 1e-9,
                    "pole on/outside unit circle: {s:?}"
                );
            }
            let len = (4 * settle_len(&c, 1e-6, 4_000_000)).max(4_096);
            let mut y = vec![0.0f32; len];
            y[0] = 1.0;
            let mut r = y.clone();
            for (bl, br) in y.chunks_mut(4_096).zip(r.chunks_mut(4_096)) {
                c.process_stereo(bl, br);
            }
            assert!(y.iter().all(|v| v.is_finite()), "non-finite: {s:?} @ {fs}");
            let q = len / 4;
            let energy = |v: &[f32]| v.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>();
            let (first, last) = (energy(&y[..q]), energy(&y[len - q..]));
            assert!(
                last <= 1e-12 * first,
                "impulse response does not decay: {s:?} @ {fs}: {first} → {last}"
            );
        }
    }
}

#[test]
fn any_block_length_is_valid() {
    let fs = 48_000.0;
    let lengths = [0usize, 1, 31, 32, 33, 2_048, 4_097, 0, 5, 64, 1, 1_000];
    let total: usize = lengths.iter().sum();
    let mut rng = Rng::new(5);
    let x: Vec<f32> = (0..total).map(|_| rng.noise(0.5)).collect();
    let mut s = all_engaged();
    s.hpf.bypass = true;

    // Reference: one block.
    let mut a = chain_with(fs, &s);
    a.set_params(&all_engaged());
    let (mut al, mut ar) = (x.clone(), x.clone());
    a.process_stereo(&mut al, &mut ar);

    let mut b = chain_with(fs, &s);
    b.set_params(&all_engaged());
    let (mut bl, mut br) = (x.clone(), x.clone());
    let mut pos = 0;
    for n in lengths {
        b.process_stereo(&mut bl[pos..pos + n], &mut br[pos..pos + n]);
        pos += n;
    }
    for (u, v) in al.iter().zip(&bl) {
        assert!((u - v).abs() <= 1e-6);
    }
    assert!(bl.iter().all(|v| v.is_finite()));
}

#[test]
fn mismatched_lengths_process_the_common_prefix() {
    let mut c = chain_with(48_000.0, &all_engaged());
    let mut l = vec![0.5f32; 100];
    let mut r = vec![0.5f32; 60];
    c.process_stereo(&mut l, &mut r);
    assert!(
        l[60..].iter().all(|&v| v == 0.5),
        "samples beyond min length touched"
    );
    assert!(l[..60].iter().any(|&v| v != 0.5));
    let mut c = chain_with(48_000.0, &all_engaged());
    let mut l = vec![0.5f32; 10];
    let mut r = vec![0.5f32; 0];
    c.process_stereo(&mut l, &mut r);
    assert!(l.iter().all(|&v| v == 0.5));
}

#[test]
fn channels_are_independent() {
    let mut rng = Rng::new(3);
    let mut c = chain_with(48_000.0, &all_engaged());
    let mut l: Vec<f32> = (0..4_096).map(|_| rng.noise(1.0)).collect();
    let mut r = vec![0.0f32; 4_096];
    c.process_stereo(&mut l, &mut r);
    assert!(r.iter().all(|&v| v == 0.0), "left leaked into right");
    let mut c = chain_with(48_000.0, &all_engaged());
    let mut l = vec![0.0f32; 4_096];
    let mut r: Vec<f32> = (0..4_096).map(|_| rng.noise(1.0)).collect();
    c.process_stereo(&mut l, &mut r);
    assert!(l.iter().all(|&v| v == 0.0), "right leaked into left");
}

#[test]
fn eq_chain_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<EqChain>();
}

// ---------------------------------------------------------------------------------
// Transparency / identity fast path
// ---------------------------------------------------------------------------------

fn tricky_input(rng: &mut Rng, len: usize) -> Vec<f32> {
    let specials = [
        0.0,
        -0.0,
        f32::MIN_POSITIVE / 8.0, // subnormal
        -f32::MIN_POSITIVE,
        1e-30,
        1.0,
        -1.0,
        1e30,
        f32::MAX,
        f32::MIN,
    ];
    (0..len)
        .map(|n| {
            if n % 97 == 0 {
                specials[rng.below(specials.len())]
            } else {
                rng.noise(1.0)
            }
        })
        .collect()
}

#[test]
fn default_settings_are_bit_exact() {
    let mut rng = Rng::new(99);
    for &fs in &ALL_RATES {
        let mut c = EqChain::new(fs);
        assert_eq!(c.active_stages(), [false; 4]);
        for f in [0.0, 20.0, 1_000.0, 20_000.0] {
            assert_eq!(c.magnitude_db(f), 0.0);
        }
        let x = tricky_input(&mut rng, 10_000);
        let x_r: Vec<f32> = x.iter().rev().copied().collect();
        let (l, r) = run_events(&mut c, &x, &x_r, &[], |rem| rng.below(rem.min(700) + 1));
        assert!(l.iter().zip(&x).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&x_r).all(|(a, b)| a.to_bits() == b.to_bits()));
    }
}

#[test]
fn zero_gain_and_bypassed_hpf_become_bit_exact_after_ramps() {
    let mut rng = Rng::new(17);
    for &fs in &TEST_RATES {
        // (a) gains set to 0 dB + HPF bypassed; (b) every band bypassed at non-zero gain.
        let mut zero = all_engaged();
        zero.hpf.bypass = true;
        zero.low_shelf.gain_db = 0.0;
        zero.peak.gain_db = 0.0;
        zero.high_shelf.gain_db = 0.0;
        let mut bypassed = all_engaged();
        bypassed.hpf.bypass = true;
        bypassed.low_shelf.bypass = true;
        bypassed.peak.bypass = true;
        bypassed.high_shelf.bypass = true;
        for target in [zero, bypassed] {
            let mut c = chain_with(fs, &all_engaged());
            let warm: Vec<f32> = (0..(0.2 * fs) as usize).map(|_| rng.noise(0.5)).collect();
            run_events(&mut c, &warm, &warm, &[], |_| 512);
            // Ramps (≈15 ms) and the decaying unity tails finish while signal flows.
            let x: Vec<f32> = (0..fs as usize).map(|_| rng.noise(0.5)).collect();
            run_events(&mut c, &x, &x, &[(0, target)], |_| 256);
            assert!(!c.is_ramping());
            assert_eq!(
                c.active_stages(),
                [false; 4],
                "bands still running: {target:?}"
            );
            for f in [20.0, 1_000.0, 15_000.0] {
                assert_eq!(c.magnitude_db(f), 0.0);
            }
            let x = tricky_input(&mut rng, 8_192);
            let (l, _) = run_events(&mut c, &x, &x, &[], |_| 333);
            assert!(l.iter().zip(&x).all(|(a, b)| a.to_bits() == b.to_bits()));
        }
    }
}

// ---------------------------------------------------------------------------------
// De-click ramps
// ---------------------------------------------------------------------------------

fn max_second_diff(v: &[f32]) -> f64 {
    v.windows(3)
        .map(|w| (f64::from(w[2]) - 2.0 * f64::from(w[1]) + f64::from(w[0])).abs())
        .fold(0.0, f64::max)
}

/// Play a sine through a sequence of settings (switched every `hold_ms`, with
/// `set_params`, or with `set_params_immediate` for the negative control) and return
/// `(max|Δy| / (G · max|Δx|), max|Δ²y| / (G · max|Δ²x|))`, where `G` is the largest
/// steady-state gain at the tone among the settings. A click is a jump the
/// steady-state signal cannot produce (ratio > 1); the second difference is the more
/// sensitive detector (a sine's Δ² is small, a step's is not).
fn step_ratios(
    fs: f64,
    tone: f64,
    states: &[EqSettings],
    hold_ms: f64,
    block: usize,
    immediate: bool,
) -> (f64, f64) {
    let mut chain = chain_with(fs, &states[0]);
    let warmup = (0.5 * fs) as usize;
    let hold = (hold_ms * 1e-3 * fs) as usize;
    let len = warmup + states.len() * hold + (0.05 * fs) as usize;
    let x = sine(fs, tone, 0.5, len);
    let mut y = x.clone();
    let mut r = x.clone();
    let mut pos = 0;
    let mut next = 1;
    while pos < len {
        if next < states.len() && pos >= warmup + (next - 1) * hold {
            if immediate {
                chain.set_params_immediate(&states[next]);
            } else {
                chain.set_params(&states[next]);
            }
            next += 1;
        }
        let mut end = (pos + block).min(len);
        if next < states.len() {
            end = end.min(warmup + (next - 1) * hold);
        }
        chain.process_stereo(&mut y[pos..end], &mut r[pos..end]);
        pos = end;
    }
    let gain = states
        .iter()
        .map(|s| 10f64.powf(chain_with(fs, s).magnitude_db(tone) / 20.0))
        .fold(0.0, f64::max);
    let from = warmup - 200;
    (
        max_step(&y[from..]) / (gain * max_step(&x[from..])),
        max_second_diff(&y[from..]) / (gain * max_second_diff(&x[from..])),
    )
}

/// One click-test scenario: a settings sequence switched every `hold_ms` on a sine.
struct ClickCase {
    name: &'static str,
    fs: f64,
    tone: f64,
    states: Vec<EqSettings>,
    hold_ms: f64,
    /// A hard switch of this sequence is clearly detectable (negative control).
    control: bool,
}

#[test]
fn bypass_and_slope_changes_are_click_free() {
    let peak = |g, q| band_only(Band::Peak, 1_000.0, g, q);
    let bypassed = |mut s: EqSettings| {
        s.hpf.bypass = true;
        s.low_shelf.bypass = true;
        s.peak.bypass = true;
        s.high_shelf.bypass = true;
        s
    };
    let slope = |mut s: EqSettings, sl| {
        s.hpf.slope = sl;
        s
    };
    // on → bypassed → on, 100 ms apart.
    let toggle = |s: EqSettings| vec![s, bypassed(s), s];
    // bypassed → on → bypassed (HPF un-bypass starts from zero state).
    let untoggle = |s: EqSettings| vec![bypassed(s), s, bypassed(s)];
    let hpf24 = hpf_only(500.0, HpfSlope::Db24);
    let hpf12 = hpf_only(300.0, HpfSlope::Db12);
    let ls = band_only(Band::LowShelf, 200.0, -18.0, 0.7);
    let hs = band_only(Band::HighShelf, 4_000.0, 18.0, 0.7);
    let slopes = vec![hpf12, slope(hpf12, HpfSlope::Db24), hpf12];
    let rapid = vec![
        all_engaged(),
        bypassed(all_engaged()),
        slope(all_engaged(), HpfSlope::Db12),
        bypassed(all_engaged()),
        all_engaged(),
        slope(bypassed(all_engaged()), HpfSlope::Db12),
        all_engaged(),
    ];
    let case = |name, fs, tone, states, hold_ms, control| ClickCase {
        name,
        fs,
        tone,
        states,
        hold_ms,
        control,
    };
    let cases = [
        case(
            "peak +18 Q1",
            48e3,
            1e3,
            toggle(peak(18.0, 1.0)),
            100.0,
            false,
        ),
        case(
            "peak +18 Q10",
            48e3,
            1e3,
            toggle(peak(18.0, 10.0)),
            100.0,
            false,
        ),
        case(
            "peak -18 Q2",
            44.1e3,
            1e3,
            toggle(peak(-18.0, 2.0)),
            100.0,
            false,
        ),
        case("low shelf -18", 48e3, 100.0, toggle(ls), 100.0, true),
        case("high shelf +18", 44.1e3, 6e3, toggle(hs), 100.0, false),
        case("HPF 24 bypass", 48e3, 200.0, untoggle(hpf24), 100.0, true),
        case("HPF 12 bypass", 96e3, 300.0, untoggle(hpf12), 100.0, true),
        case("HPF 12<->24", 48e3, 200.0, slopes, 100.0, true),
        case("all bands", 48e3, 1e3, toggle(all_engaged()), 100.0, false),
        case(
            "rapid toggles (mid-ramp reversal)",
            48e3,
            700.0,
            rapid,
            4.0,
            true,
        ),
    ];
    let (mut worst_d1, mut worst_d2) = (0.0f64, 0.0f64);
    for c in &cases {
        let name = c.name;
        for block in [1, 33, 512] {
            let (d1, d2) = step_ratios(c.fs, c.tone, &c.states, c.hold_ms, block, false);
            assert!(d1 < 1.1, "{name} (block {block}): Δ ratio {d1:.3}");
            assert!(d2 < 1.25, "{name} (block {block}): Δ² ratio {d2:.3}");
            (worst_d1, worst_d2) = (worst_d1.max(d1), worst_d2.max(d2));
            if c.control && block == 512 {
                // Negative control: the detector does see a hard switch.
                let (h1, h2) = step_ratios(c.fs, c.tone, &c.states, c.hold_ms, block, true);
                println!("click test {name:<34}: Δ² ratio {d2:.3} (hard switch {h1:.2}, {h2:.1})");
                assert!(h2 > 5.0, "{name}: control not detected ({h2:.3})");
            }
        }
    }
    println!("click test: worst Δ ratio {worst_d1:.3}, worst Δ² ratio {worst_d2:.3}");
}

#[test]
fn ramps_finish_within_20ms() {
    let mut rng = Rng::new(21);
    for &fs in &ALL_RATES {
        let mut c = chain_with(fs, &flat());
        let n = (0.020 * fs) as usize;
        let mut toggled = all_engaged();
        for step in 0..3 {
            match step {
                // HPF un-bypass (+ shelves engaged directly).
                0 => {}
                // Bypass everything at non-zero gain.
                1 => {
                    toggled.hpf.bypass = true;
                    toggled.low_shelf.bypass = true;
                    toggled.peak.bypass = true;
                    toggled.high_shelf.bypass = true;
                }
                // Re-engage everything and switch the slope.
                _ => {
                    toggled = all_engaged();
                    toggled.hpf.slope = HpfSlope::Db12;
                }
            }
            c.set_params(&toggled);
            assert!(c.is_ramping(), "step {step}: no ramp started");
            let x: Vec<f32> = (0..n).map(|_| rng.noise(0.5)).collect();
            run_events(&mut c, &x, &x, &[], |_| 100);
            assert!(
                !c.is_ramping(),
                "step {step}: ramp still running after 20 ms at {fs}"
            );
        }
    }
}

#[test]
fn chunked_processing_matches_one_block() {
    let mut rng = Rng::new(7);
    for &fs in &TEST_RATES {
        for _ in 0..4 {
            let len = (0.5 * fs) as usize;
            let x_l: Vec<f32> = (0..len).map(|_| rng.noise(0.5)).collect();
            let x_r: Vec<f32> = (0..len).map(|_| rng.noise(0.5)).collect();
            let start = random_settings(&mut rng);
            let mut s = start;
            let mut events = Vec::new();
            let mut pos = rng.below(300);
            while pos < len {
                match rng.below(3) {
                    0 => {
                        // Discrete toggles (bypass/slope ramps).
                        s.hpf.bypass ^= rng.coin();
                        s.low_shelf.bypass ^= rng.coin();
                        s.peak.bypass ^= rng.coin();
                        s.high_shelf.bypass ^= rng.coin();
                        if rng.coin() {
                            s.hpf.slope = if s.hpf.slope == HpfSlope::Db12 {
                                HpfSlope::Db24
                            } else {
                                HpfSlope::Db12
                            };
                        }
                        events.push((pos, s));
                        pos += 1 + rng.below(1_500);
                    }
                    1 => {
                        // A smoothing run: new settings every 32 samples.
                        for _ in 0..20 {
                            s.hpf.freq_hz = (s.hpf.freq_hz * 1.02).min(500.0);
                            s.low_shelf.gain_db = (s.low_shelf.gain_db + 0.4).min(18.0);
                            s.peak.freq_hz = (s.peak.freq_hz * 0.98).max(100.0);
                            s.peak.q = (s.peak.q * 1.03).min(10.0);
                            s.high_shelf.gain_db = (s.high_shelf.gain_db - 0.3).max(-18.0);
                            events.push((pos, s));
                            pos += SUB_BLOCK_SIZE;
                        }
                    }
                    _ => {
                        s = random_settings(&mut rng);
                        events.push((pos, s));
                        pos += 1 + rng.below(2_000);
                    }
                }
            }
            let mut a = chain_with(fs, &start);
            let (al, ar) = run_events(&mut a, &x_l, &x_r, &events, |rem| rem);
            let mut b = chain_with(fs, &start);
            let (bl, br) = run_events(&mut b, &x_l, &x_r, &events, |_| rng.below(98));
            let max_diff = al
                .iter()
                .zip(&bl)
                .chain(ar.iter().zip(&br))
                .map(|(u, v)| (u - v).abs())
                .fold(0.0f32, f32::max);
            assert!(max_diff <= 1e-6, "chunked output differs by {max_diff}");
        }
    }
}

// ---------------------------------------------------------------------------------
// Denormals
// ---------------------------------------------------------------------------------

#[test]
fn flush_denormals_zeroes_exactly_the_subnormals() {
    let smallest = f32::from_bits(1);
    let largest_subnormal = f32::from_bits(0x007f_ffff);
    let cases: [(f32, f32); 14] = [
        (smallest, 0.0),
        (-smallest, -0.0),
        (largest_subnormal, 0.0),
        (-largest_subnormal, -0.0),
        (f32::MIN_POSITIVE / 3.0, 0.0),
        (f32::MIN_POSITIVE, f32::MIN_POSITIVE),
        (-f32::MIN_POSITIVE, -f32::MIN_POSITIVE),
        (0.0, 0.0),
        (-0.0, -0.0),
        (1e-30, 1e-30),
        (-0.5, -0.5),
        (f32::MAX, f32::MAX),
        (f32::INFINITY, f32::INFINITY),
        (f32::NEG_INFINITY, f32::NEG_INFINITY),
    ];
    // Odd length, so a vectorized loop's scalar tail is exercised too.
    let mut buf: Vec<f32> = cases.iter().map(|&(x, _)| x).chain([f32::NAN]).collect();
    flush_denormals(&mut buf);
    for (&(x, expected), &y) in cases.iter().zip(&buf) {
        assert_eq!(y.to_bits(), expected.to_bits(), "{x:e} → {y:e}");
    }
    assert!(buf[cases.len()].is_nan());
    flush_denormals(&mut []);

    // Random bit patterns: subnormal ⇔ changed, and changed only to a signed zero.
    let mut rng = Rng::new(11);
    let mut v: Vec<f32> = (0..4_099)
        .map(|_| f32::from_bits(rng.next_u64() as u32 >> (rng.below(2) * 8)))
        .collect();
    let before = v.clone();
    flush_denormals(&mut v);
    for (a, b) in before.iter().zip(&v) {
        if a.is_subnormal() {
            assert_eq!(*b, 0.0);
            assert_eq!(b.is_sign_negative(), a.is_sign_negative());
        } else {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }
    assert!(before.iter().any(|a| a.is_subnormal()));
}

#[test]
fn silence_after_impulse_flushes_to_exact_zero() {
    for &fs in &TEST_RATES {
        for variant in 0..2 {
            let mut c = chain_with(fs, &all_engaged());
            let block = 512;
            let mut l = vec![0.0f32; block];
            let mut r = vec![0.0f32; block];
            l[0] = 1.0;
            r[0] = -1.0;
            c.process_stereo(&mut l, &mut r);
            if variant == 1 {
                // Bypass everything right after the impulse: ramps + unity tails.
                let mut s = all_engaged();
                s.hpf.bypass = true;
                s.low_shelf.bypass = true;
                s.peak.gain_db = 0.0;
                s.high_shelf.bypass = true;
                c.set_params(&s);
            }
            let blocks = (10.0 * fs) as usize / block;
            for _ in 0..blocks {
                l.fill(0.0);
                r.fill(0.0);
                c.process_stereo(&mut l, &mut r);
                for v in c.all_state_values() {
                    assert!(
                        v == 0.0 || v.abs() >= biquad::DENORMAL_THRESHOLD,
                        "state {v:e} not flushed"
                    );
                    assert!(!v.is_subnormal());
                }
            }
            assert!(c.all_state_values().iter().all(|&v| v == 0.0));
            assert!(l.iter().chain(&r).all(|&v| v == 0.0));
        }
    }
}

// ---------------------------------------------------------------------------------
// Coefficient recomputation
// ---------------------------------------------------------------------------------

#[test]
fn coefficients_recompute_only_on_change() {
    let fs = 48_000.0;
    let mut c = EqChain::new(fs);
    let mut l = vec![0.1f32; 4_096];
    let mut r = l.clone();
    let base = c.coeff_recomputes();

    // All four bands change → four designs (the HPF counts as one band).
    let s = all_engaged();
    c.set_params(&s);
    assert_eq!(c.coeff_recomputes(), base + 4);
    // Identical settings: nothing.
    c.set_params(&s);
    assert_eq!(c.coeff_recomputes(), base + 4);
    // Processing (HPF un-bypass crossfade only) recomputes nothing.
    for _ in 0..10 {
        c.process_stereo(&mut l, &mut r);
    }
    assert_eq!(c.coeff_recomputes(), base + 4);

    // One band's gain changes → exactly one design.
    let mut s2 = s;
    s2.peak.gain_db = 3.0;
    c.set_params(&s2);
    assert_eq!(c.coeff_recomputes(), base + 5);

    // HPF slope / bypass toggles never redesign (crossfades only).
    let mut s3 = s2;
    s3.hpf.slope = HpfSlope::Db12;
    c.set_params(&s3);
    s3.hpf.bypass = true;
    c.set_params(&s3);
    for _ in 0..10 {
        c.process_stereo(&mut l, &mut r);
    }
    assert_eq!(c.coeff_recomputes(), base + 5);

    // A shelf bypass toggle: nothing in set_params, one design per ramp sub-block,
    // then nothing once the ramp is done.
    let mut s4 = s3;
    s4.low_shelf.bypass = true;
    c.set_params(&s4);
    assert_eq!(c.coeff_recomputes(), base + 5);
    for _ in 0..10 {
        c.process_stereo(&mut l, &mut r);
    }
    let after_ramp = c.coeff_recomputes();
    assert_eq!(after_ramp, base + 5 + c.ramp_sub_blocks());
    for _ in 0..10 {
        c.set_params(&s4);
        c.process_stereo(&mut l, &mut r);
    }
    assert_eq!(c.coeff_recomputes(), after_ramp);

    // Bypass toggle of a 0 dB band: no ramp, no design.
    let mut s5 = s4;
    s5.peak.gain_db = 0.0;
    c.set_params(&s5);
    let n = c.coeff_recomputes();
    s5.peak.bypass = true;
    c.set_params(&s5);
    assert!(!c.is_ramping());
    assert_eq!(c.coeff_recomputes(), n);

    // set_params_immediate with unchanged settings: nothing.
    c.set_params_immediate(&s5);
    assert_eq!(c.coeff_recomputes(), n);
}
