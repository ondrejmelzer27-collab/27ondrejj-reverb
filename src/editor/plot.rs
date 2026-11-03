//! Plot coordinate mapping (log frequency axis, dB axes) and grid definitions.
//!
//! Pure functions on plain `egui` geometry types, unit tested without a GUI context.

use egui::{Pos2, Rect, pos2};

/// Lowest frequency on the x axis.
pub const FREQ_MIN: f32 = 20.0;
/// Highest frequency on the x axis.
pub const FREQ_MAX: f32 = 20_000.0;
/// The EQ curve's vertical range is `-EQ_DB_RANGE..=EQ_DB_RANGE`.
pub const EQ_DB_RANGE: f32 = 24.0;
/// EQ grid spacing in dB.
pub const EQ_DB_STEP: f32 = 6.0;
/// Analyzer level mapped to the bottom of the plot.
pub const ANALYZER_DB_MIN: f32 = -96.0;
/// Analyzer level mapped to the top of the plot.
pub const ANALYZER_DB_MAX: f32 = 0.0;

/// Vertical grid lines: decades plus 2× and 5× lines.
pub const GRID_FREQS: [f32; 10] = [
    20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0, 20_000.0,
];
/// Labeled frequencies.
pub const FREQ_LABELS: [(f32, &str); 8] = [
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1_000.0, "1k"),
    (2_000.0, "2k"),
    (5_000.0, "5k"),
    (10_000.0, "10k"),
];

/// `true` for the decade lines (drawn slightly stronger).
pub fn is_decade(freq: f32) -> bool {
    let exponent = freq.log10();
    (exponent - exponent.round()).abs() < 1e-4
}

/// Position on the log frequency axis in `0..=1` (unclamped outside the axis range).
pub fn freq_to_norm(freq: f32) -> f32 {
    (freq.max(1e-3) / FREQ_MIN).ln() / (FREQ_MAX / FREQ_MIN).ln()
}

/// Inverse of [`freq_to_norm`].
pub fn norm_to_freq(t: f32) -> f32 {
    FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(t)
}

/// Maps between plot-space values (Hz, dB) and screen positions inside `rect`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlotMapping {
    pub rect: Rect,
}

impl PlotMapping {
    pub fn new(rect: Rect) -> Self {
        Self { rect }
    }

    fn width(&self) -> f32 {
        self.rect.width().max(1.0)
    }

    fn height(&self) -> f32 {
        self.rect.height().max(1.0)
    }

    pub fn freq_to_x(&self, freq: f32) -> f32 {
        self.rect.left() + freq_to_norm(freq) * self.width()
    }

    pub fn x_to_freq(&self, x: f32) -> f32 {
        norm_to_freq((x - self.rect.left()) / self.width())
    }

    /// EQ gain → y (+`EQ_DB_RANGE` at the top, −`EQ_DB_RANGE` at the bottom).
    pub fn db_to_y(&self, db: f32) -> f32 {
        self.rect.center().y - db / (2.0 * EQ_DB_RANGE) * self.height()
    }

    pub fn y_to_db(&self, y: f32) -> f32 {
        (self.rect.center().y - y) / self.height() * (2.0 * EQ_DB_RANGE)
    }

    /// Analyzer level (dBFS) → y, clamped to the plot.
    pub fn analyzer_db_to_y(&self, db: f32) -> f32 {
        let t = ((db - ANALYZER_DB_MIN) / (ANALYZER_DB_MAX - ANALYZER_DB_MIN)).clamp(0.0, 1.0);
        self.rect.bottom() - t * self.height()
    }

    /// Screen position of (`freq`, `db`).
    pub fn pos(&self, freq: f32, db: f32) -> Pos2 {
        pos2(self.freq_to_x(freq), self.db_to_y(db))
    }
}

/// Human-readable frequency, e.g. `"85 Hz"`, `"1.20 kHz"`, `"12.5 kHz"`.
pub fn format_freq(freq: f32) -> String {
    if freq < 999.5 {
        format!("{freq:.0} Hz")
    } else if freq < 9_995.0 {
        format!("{:.2} kHz", freq / 1_000.0)
    } else {
        format!("{:.1} kHz", freq / 1_000.0)
    }
}

/// Signed gain, e.g. `"+3.5 dB"`.
pub fn format_gain(db: f32) -> String {
    // Avoid printing "-0.0".
    let db = if db.abs() < 0.05 { 0.0 } else { db };
    format!("{db:+.1} dB")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn mapping() -> PlotMapping {
        PlotMapping::new(Rect::from_min_size(pos2(10.0, 20.0), vec2(800.0, 400.0)))
    }

    #[test]
    fn freq_axis_endpoints() {
        let m = mapping();
        assert!((m.freq_to_x(FREQ_MIN) - 10.0).abs() < 1e-3);
        assert!((m.freq_to_x(FREQ_MAX) - 810.0).abs() < 1e-3);
        // 632 Hz is the geometric centre of 20 Hz..20 kHz.
        assert!((m.freq_to_x((FREQ_MIN * FREQ_MAX).sqrt()) - 410.0).abs() < 1e-2);
    }

    #[test]
    fn freq_x_roundtrip() {
        let m = mapping();
        for freq in [
            20.0, 37.0, 100.0, 440.0, 1_000.0, 3_150.0, 12_000.0, 20_000.0,
        ] {
            let back = m.x_to_freq(m.freq_to_x(freq));
            assert!((back / freq - 1.0).abs() < 1e-4, "{freq} -> {back}");
        }
        for x in [10.0, 100.0, 333.3, 810.0] {
            assert!((m.freq_to_x(m.x_to_freq(x)) - x).abs() < 1e-2);
        }
    }

    #[test]
    fn db_y_roundtrip_and_orientation() {
        let m = mapping();
        assert!((m.db_to_y(EQ_DB_RANGE) - 20.0).abs() < 1e-4);
        assert!((m.db_to_y(-EQ_DB_RANGE) - 420.0).abs() < 1e-4);
        assert!((m.db_to_y(0.0) - 220.0).abs() < 1e-4);
        for db in [-24.0, -18.0, -3.5, 0.0, 0.1, 6.0, 18.0, 24.0] {
            assert!((m.y_to_db(m.db_to_y(db)) - db).abs() < 1e-4);
        }
    }

    #[test]
    fn analyzer_mapping_is_clamped() {
        let m = mapping();
        assert!((m.analyzer_db_to_y(ANALYZER_DB_MAX) - 20.0).abs() < 1e-4);
        assert!((m.analyzer_db_to_y(ANALYZER_DB_MIN) - 420.0).abs() < 1e-4);
        assert!((m.analyzer_db_to_y(-48.0) - 220.0).abs() < 1e-4);
        assert_eq!(m.analyzer_db_to_y(-200.0), 420.0);
        assert_eq!(m.analyzer_db_to_y(20.0), 20.0);
    }

    #[test]
    fn degenerate_rect_does_not_divide_by_zero() {
        let m = PlotMapping::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 0.0)));
        assert!(m.x_to_freq(5.0).is_finite());
        assert!(m.y_to_db(5.0).is_finite());
    }

    #[test]
    fn grid_and_formatting() {
        assert!(is_decade(100.0) && is_decade(1_000.0) && is_decade(10_000.0));
        assert!(!is_decade(200.0) && !is_decade(5_000.0));
        assert_eq!(format_freq(85.2), "85 Hz");
        assert_eq!(format_freq(1_200.0), "1.20 kHz");
        assert_eq!(format_freq(12_500.0), "12.5 kHz");
        assert_eq!(format_gain(3.46), "+3.5 dB");
        assert_eq!(format_gain(-0.01), "+0.0 dB");
        assert_eq!(format_gain(-6.0), "-6.0 dB");
    }
}
