# vocal_eq — project rules

Rust audio plugin (CLAP + VST3, egui editor) built on **nice-plug** (the community
fork of NIH-plug, crates.io `nice-plug` 0.4.x + `nice-plug-egui` 0.6.x).
Do **not** use `nih_plug`, robbert-vdh/nih-plug, or any codeberg/git URL for the
framework — crates.io only.

Target platform is macOS (Logic needs AU, which is not supported — test in Reaper,
Bitwig, Ableton, FL Studio). Development/CI also builds and tests on Linux.

## Commands

```bash
cargo build                                  # debug build (assert_process_allocs on)
cargo test                                   # all unit tests (DSP tests must pass)
cargo clippy --all-targets -- -D warnings    # lint
cargo xtask bundle vocal_eq --release        # -> target/bundled/vocal_eq.{clap,vst3}
scripts/install.sh                           # build + (codesign on macOS) + install
```

## Layout and ownership

| Path                    | Purpose                                                        |
|-------------------------|----------------------------------------------------------------|
| `src/lib.rs`            | Plugin impl, CLAP/VST3 export, `initialize`/`reset`/`process`  |
| `src/params.rs`         | **Shared contract**: all parameters + smoothers                |
| `src/analyzer_bus.rs`   | **Shared contract**: lock-free SPSC audio→GUI sample transport |
| `src/dsp/**`            | Pure DSP (biquads, bands, `EqChain`). No nice-plug imports.    |
| `src/editor/**`         | egui editor (controls, EQ curve, nodes)                        |
| `src/analyzer/**`       | GUI-thread spectrum analyzer (FFT, smoothing, tilt)            |
| `xtask/`                | Bundler entry point (`nice-plug-xtask`)                        |
| `scripts/install.sh`    | Release build + ad-hoc codesign + install                      |

Changes to the shared contract files (`params.rs`, `analyzer_bus.rs`, the public API
in `dsp/mod.rs`) must be coordinated — other modules code against them.

## Realtime-safety rules (audio thread: `process()` and everything it calls)

1. **No allocations.** No `Vec::push`/`collect`/`Box::new`/`String`/`format!`/`clone()`
   of heap types, no `Arc::new`. All buffers are allocated in `initialize()` (or
   `new()`), sized for the maximum block size / channel count. Debug builds enable
   nice-plug's `assert_process_allocs` feature, which panics on any allocation in
   `process()` — keep it that way.
2. **No locks.** No `Mutex`, `RwLock`, blocking channel ops. Use atomics, the
   framework's smoothers, and the `rtrb` SPSC ring buffer in `analyzer_bus.rs`
   (push only, never block; drop samples if the GUI is not keeping up).
3. **No I/O.** No `println!`, logging macros, file or network access in `process()`.
4. **No unbounded loops.** Every loop is bounded by the buffer length, a fixed
   sub-block size, or a compile-time constant.
5. **No panics in normal operation.** Avoid `unwrap()`/indexing that can fail on
   valid host input; assume any block size from 1 to `max_buffer_size`.

## DSP rules

- **Smoothing for every audio parameter.** All continuous params (freqs, gains, Qs,
  output gain) have smoothers in `params.rs`. Bypass toggles must not click: cross-
  fade or ramp the band gain to 0 dB rather than switching hard.
- **Coefficient updates** only while a smoother is active, in fixed **32-sample
  sub-blocks**; when nothing is smoothing, coefficients are not recomputed.
- **Biquads**: RBJ Audio EQ Cookbook formulas, Transposed Direct Form II,
  `f64` state and coefficients, `f32` I/O.
- **Denormal protection**: flush tiny state values to zero (or add/subtract a tiny
  DC offset) in filter state updates, and/or enable FTZ/DAZ for the duration of
  `process()`. Silence in must not cause CPU spikes.
- `src/dsp/` is **independent of nice-plug**: plain Rust + `std` only, so it can be
  unit tested and reused without a host. The plugin layer converts params to plain
  DSP settings structs.
- **Unit tests for all DSP**: measured response vs analytic magnitude (within
  0.1 dB), no NaN/inf, stability at extreme settings, 0 dB gain = transparent,
  sample rates 44.1/48/96 kHz.

## GUI rules

- GUI code never blocks the audio thread: it only reads params (atomics) and pops
  from the analyzer ring buffer.
- FFT and all analysis run on the GUI thread; preallocate FFT buffers once.
- Use `magnitude_db` / `band_magnitude_db` from `src/dsp` (on a GUI-owned `EqChain`
  copy updated from params) for curve drawing — never touch the audio thread's chain.

## Style

- `cargo fmt`, no clippy warnings, no `unsafe` outside documented FTZ/denormal code.
- Keep commits focused; tests must pass before committing.
