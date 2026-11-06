# Vocal EQ

`vocal_eq` is a stereo four-band EQ plugin for vocals, available as CLAP and VST3. It has a
high-pass filter, a low shelf, a mid peak and a high shelf. The resizable egui editor shows
the EQ curve with draggable band nodes over a real-time spectrum analyzer. The plugin is
written in Rust on [nice-plug](https://crates.io/crates/nice-plug) (the community fork of
NIH-plug) and `nice-plug-egui`.

## Features

- **4 bands:** high-pass (12 or 24 dB/oct Butterworth), low shelf, mid peak and high shelf,
  each with its own bypass. Output gain of ±24 dB.
- **Clean DSP:** RBJ Audio EQ Cookbook biquads in Transposed Direct Form II with `f64` state
  and coefficients. Continuous parameters are smoothed over 50 ms, and bypass and slope
  changes are de-clicked.
- **Bit-exact pass-through at default settings:** bands that do nothing are skipped.
- **Realtime-safe audio thread:** no allocations, locks or I/O. Debug builds enforce this
  with nice-plug's `assert_process_allocs`.
- **Spectrum analyzer:** 4096-point FFT, Hann window, 50 % overlap, temporal smoothing and an
  optional 4.5 dB/oct display tilt.
- **Interactive EQ display:** drag nodes to set frequency and gain, use the mouse wheel to set
  Q, and double-click to bypass. The editor is resizable and remembers its size.
- **Stable parameter IDs,** so saved projects and presets keep working across versions.

## Requirements

- **Rust stable ≥ 1.95** (`rust-version` in `Cargo.toml`, which is egui 0.37's MSRV),
  installed via [rustup](https://rustup.rs). The crate uses edition 2024.
- **macOS:** Xcode Command Line Tools (`xcode-select --install`).
- **Linux:** X11/XCB/OpenGL/xkbcommon development packages. On Debian/Ubuntu:

  ```bash
  sudo apt install pkg-config libx11-dev libx11-xcb-dev libxcursor-dev libxcb1-dev \
      libxcb-icccm4-dev libxcb-dri2-0-dev libgl-dev libxkbcommon-x11-dev
  ```

  `libasound2-dev` and `libjack-jackd2-dev` (often listed for NIH-plug) are only needed for
  nice-plug's `standalone` feature, which this project does not enable.

## Building

```bash
cargo xtask bundle vocal_eq --release   # -> target/bundled/vocal_eq.clap, vocal_eq.vst3
cargo test                              # unit tests
```

`cargo xtask` is a Cargo alias defined in `.cargo/config.toml`. It runs the `xtask` workspace
member, which is the `nice-plug-xtask` bundler. On macOS you can optionally build a universal
(x86_64 + arm64) binary, which `nice-plug-xtask` combines with `lipo`:

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo xtask bundle-universal vocal_eq --release
```

## Installing

```bash
scripts/install.sh            # release build + install
scripts/install.sh --no-build # install the bundles already in target/bundled/
```

- **macOS:** the script builds the release bundles and ad-hoc signs them
  (`codesign --force --deep -s -`). It then copies them to `~/Library/Audio/Plug-Ins/CLAP` and
  `~/Library/Audio/Plug-Ins/VST3`, replacing older copies, and checks the installed signatures.
- **Linux:** it copies the bundles to `~/.clap` and `~/.vst3`, with no signing.
- **Manual install:** copy `target/bundled/vocal_eq.clap` and `vocal_eq.vst3` into the same
  folders. On macOS, first run `codesign --force --deep -s - target/bundled/vocal_eq.{clap,vst3}`.
  Then rescan plug-ins in your DAW.
- **Gatekeeper (macOS):** bundles you build and ad-hoc sign yourself generally load. If a
  bundle downloaded from elsewhere is blocked, remove its quarantine attribute with
  `xattr -dr com.apple.quarantine <path/to/vocal_eq.vst3>` (or the `.clap`).

## Host compatibility

- **Logic Pro (and GarageBand) require Audio Units (AU), which this plugin does not provide.**
  For testing, use **Reaper**, **Bitwig Studio**, **Ableton Live** or **FL Studio**.
- Use **CLAP** where the host supports it (Reaper and Bitwig Studio do). Use **VST3** in hosts
  without CLAP support.
- The plugin has one stereo input and one stereo output (no mono layout).
- Identifiers: CLAP ID `com.vocal-eq.vocal-eq` and VST3 class ID `VocalEqPlugin01A`.
  Categories: Audio Effect / Equalizer / Stereo (CLAP) and Fx / EQ (VST3).

## Parameters

**Parameter IDs are part of the saved state and never change**, so projects and presets stay
compatible. All parameters can be automated.

| ID | Name | Range | Default | Smoothing |
|----|------|-------|---------|-----------|
| `hpf_freq` | HPF Freq | 20 – 500 Hz | 80 Hz | 50 ms, logarithmic |
| `hpf_slope` | HPF Slope | 12 / 24 dB/oct | 12 dB/oct | ≈15 ms crossfade |
| `hpf_bypass` | HPF Bypass | on / off | **on** (HPF bypassed) | ≈15 ms crossfade |
| `ls_freq` | Low Shelf Freq | 20 – 2000 Hz | 150 Hz | 50 ms, logarithmic |
| `ls_gain` | Low Shelf Gain | ±18 dB (0.1 dB steps) | 0 dB | 50 ms, linear |
| `ls_q` | Low Shelf Q | 0.3 – 2.0 | 0.707 (1/√2) | 50 ms, logarithmic |
| `ls_bypass` | Low Shelf Bypass | on / off | off | ≈15 ms ramp |
| `pk_freq` | Mid Peak Freq | 100 – 12000 Hz | 1000 Hz | 50 ms, logarithmic |
| `pk_gain` | Mid Peak Gain | ±18 dB (0.1 dB steps) | 0 dB | 50 ms, linear |
| `pk_q` | Mid Peak Q | 0.1 – 10 | 1.0 | 50 ms, logarithmic |
| `pk_bypass` | Mid Peak Bypass | on / off | off | ≈15 ms ramp |
| `hs_freq` | High Shelf Freq | 1000 – 20000 Hz | 10 kHz | 50 ms, logarithmic |
| `hs_gain` | High Shelf Gain | ±18 dB (0.1 dB steps) | 0 dB | 50 ms, linear |
| `hs_q` | High Shelf Q | 0.3 – 2.0 | 0.707 (1/√2) | 50 ms, logarithmic |
| `hs_bypass` | High Shelf Bypass | on / off | off | ≈15 ms ramp |
| `out_gain` | Output Gain | ±24 dB | 0 dB | 50 ms, logarithmic, per sample |

- Frequency and Q controls use skewed (log-like) ranges.
- `out_gain` is stored as a linear factor and displayed in dB.
- Shelf Q is the cookbook's shelf Q, where 1/√2 corresponds to S = 1.
- Slope and bypass are discrete. The DSP de-clicks them internally rather than smoothing them.
- The plugin state also saves three non-automatable values: `editor-width` and
  `editor-height` (default 960 × 600) and `analyzer-tilt` (default on).

## DSP overview

```
in (stereo) ─► HPF ─► low shelf ─► mid peak ─► high shelf ─► output gain ─► out
                                                                    └─► (L+R)/2 ─► analyzer ring
```

- **Filters:** [RBJ Audio EQ Cookbook](https://www.w3.org/TR/audio-eq-cookbook/) designs
  (HPF, lowShelf/highShelf in Q form, peakingEQ). They run as Transposed Direct Form II
  biquads with `f64` coefficients and state, and `f32` I/O. Each sample stays in `f64` through
  all bands.
  - Coefficients are normalized by dividing by `a0`, so a 0 dB band is exactly unity.
  - Design inputs are clamped (10 Hz – 0.49·fs, Q 0.05 – 50, ±48 dB), so the filters are
    always finite and stable.
- **High-pass:** 12 dB/oct is a 2nd-order Butterworth (one biquad, Q = 1/√2). 24 dB/oct is a
  4th-order Butterworth: two cascaded biquads with Q = 0.5412 and 1.3066. While the HPF is
  engaged both paths run, so a slope change crossfades between two settled signals.
- **Smoothing:** frequency, gain and Q smooth over 50 ms. While any EQ smoother is active, the
  block is processed in **32-sample sub-blocks**, and smoothers and coefficients are updated
  once per sub-block. When nothing is smoothing, no coefficients are recomputed. Output gain
  is smoothed per sample.
- **Click-free bypass and slope:** a ≈15 ms ramp, rounded to whole sub-blocks.
  - Shelf/peak bypass scales the band's dB gain towards 0, with coefficients interpolated per
    sample.
  - HPF bypass and slope use equal-gain smoothstep crossfades (dry ↔ filtered, 12 ↔ 24).
  - The result does not depend on the host's block size.
- **Bit-exact transparency:** a band at exactly 0 dB with zero state is skipped. A fully
  bypassed HPF is skipped and its state cleared. Unity output gain is not applied. At the
  default settings the buffers are not touched at all. When a band reaches 0 dB, its decaying
  tail runs out until it falls below 1e-12 (−240 dBFS) and is then dropped, so there is no
  step.
- **Denormals:** subnormal samples are zeroed at the plugin's input and output with an
  integer bit test (no FTZ/DAZ flags, no `unsafe`), so the host never receives subnormals and
  subnormal input doesn't hit the slow CPU path. Filter state below 1e-20 is flushed to zero
  once per processed segment, so silence never leaves subnormals behind. NaN or inf state is
  reset, so one bad input sample cannot poison a filter.
- **Realtime safety:** `process()` has no allocations, locks, I/O or unbounded loops.
  - The analyzer transport is a lock-free SPSC [`rtrb`](https://crates.io/crates/rtrb) ring
    of 32768 samples, preallocated at construction. The audio thread drops samples when it is
    full.
  - Debug builds enable nice-plug's `assert_process_allocs`, which aborts with a backtrace on
    any allocation in `process()`.

**Spectrum analyzer.** The analyzer runs entirely on the GUI thread. Each frame, the editor
drains the ring and runs a 4096-point [`realfft`](https://crates.io/crates/realfft) FFT
(periodic Hann window, hop 2048 = 50 % overlap, at most 4 FFTs per frame, newest audio
first).

- Magnitudes are in dBFS, scaled so a full-scale sine reads 0 dBFS. The display range is
  −96 to 0 dBFS.
- Smoothing uses a 10 ms attack and a 300 ms release, independent of frame rate. The display
  decays when audio stops.
- Bins are mapped to log-spaced columns from 20 Hz to 20 kHz.
- The optional **+4.5 dB/oct tilt** (pivot at 1 kHz) affects the display only.
- EQ curves are evaluated analytically on a separate GUI-owned `EqChain`, never on the audio
  thread's filters.

## Using the editor

The **EQ display** has a log frequency axis (20 Hz – 20 kHz), EQ gain on the left (±24 dB,
grid every 6 dB) and the analyzer's dBFS scale on the right. Nodes **1–4** are the high-pass,
low shelf, mid peak and high shelf.

| Action | Effect |
|--------|--------|
| Drag a node | Horizontal sets frequency, vertical sets gain. The HPF node moves horizontally only. |
| Mouse wheel / scroll over a node | Changes Q by ×1.12 per notch. Also works while dragging. No effect on the HPF. |
| Double-click a node | Toggles bypass. Bypassed nodes are drawn hollow. |
| Hover a node | Shows a readout: name, frequency, gain, Q (or slope) and bypass state. |

Every edit is a complete begin/set/end gesture, so host automation recording and undo work.

The **control strip** at the bottom has one group per band:

- **High-Pass:** frequency slider, **12 dB** / **24 dB** buttons and an **ON/OFF** button.
- **Low Shelf, Mid Peak, High Shelf:** FREQ, GAIN and Q sliders and an **ON/OFF** button.
- **Output:** gain slider and the **Tilt 4.5 dB/oct** analyzer toggle.

The sliders are nice-plug-egui `ParamSlider`s. Double-click or Ctrl/Cmd-click resets a slider,
Shift-drag adjusts it finely, and clicking the value text lets you type a value.

To **resize**, drag the bottom-right corner of the window. The minimum is 640 × 420 and the
default is 960 × 600. The size is saved with the plugin state and restored when the editor
reopens.

## Project layout

```
Cargo.toml            crate manifest (cdylib + lib), workspace with xtask
.cargo/config.toml    `cargo xtask` alias
CLAUDE.md             project rules (realtime safety, DSP, GUI, style)
scripts/install.sh    release build + ad-hoc codesign (macOS) + install
xtask/                bundler entry point (nice-plug-xtask)
src/
  lib.rs              plugin impl, CLAP/VST3 export, process()
  params.rs           parameters + smoothers (shared contract)
  analyzer_bus.rs     lock-free audio → GUI sample ring (shared contract)
  dsp/                pure DSP, std only: biquad.rs, band.rs, mod.rs (EqChain), tests.rs
  analyzer/           GUI-thread FFT spectrum analyzer
  editor/             egui editor: mod.rs (layout, size persistence), view, plot, nodes, controls, theme
```

## Development

Read [`CLAUDE.md`](CLAUDE.md) first. It contains the realtime-safety, DSP and GUI rules.

```bash
cargo build                                  # debug build: any allocation in process() aborts
cargo test                                   # DSP, analyzer, params and editor unit tests
cargo clippy --all-targets -- -D warnings    # must be clean
cargo fmt
cargo build --profile profiling              # release optimizations + debug symbols
```

- `src/dsp/` has no nice-plug dependency. Its tests check the measured response against the
  analytic response (within 0.1 dB), stability and NaN/inf at extreme settings, bit-exact
  transparency, click-free bypass and slope changes, and independence from block size, at
  44.1, 48 and 96 kHz.
- `params.rs`, `analyzer_bus.rs` and the public API of `dsp/mod.rs` are shared contracts.
  Coordinate any change to them.
- Use only the crates.io `nice-plug*` crates. Do not use `nih_plug` or git or Codeberg URLs.

## Licenses

**The project currently has no `LICENSE` file.** The owner has yet to choose a license, so
none is granted for now.

| Dependency | License |
|------------|---------|
| `nice-plug`, `nice-plug-core`, `nice-plug-derive`, `nice-plug-egui`, `nice-plug-xtask` | ISC |
| `vst3` (VST3 bindings used by nice-plug) | MIT OR Apache-2.0 |
| `egui`, `baseview`, `egui-baseview`, `rtrb`, `clap-sys` | MIT / Apache-2.0 |
| `realfft` | MIT |
| `nice-assert-no-alloc` (debug builds only) | BSD-1-Clause |

nice-plug exports VST3 through the `vst3` crate. Its bindings are generated from Steinberg's
VST3 SDK headers, which Steinberg has relicensed under MIT. Unlike the older GPLv3
`vst3-sys` bindings, this brings no GPLv3 obligation. Check each dependency's license before
distributing binaries.
