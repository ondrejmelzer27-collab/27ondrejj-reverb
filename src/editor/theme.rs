//! Colors and egui style for the editor (dark theme).

use egui::{Color32, Context, CornerRadius, Stroke, Theme, Visuals, vec2};

use crate::dsp::Band;

/// Window background.
pub const BG: Color32 = Color32::from_rgb(0x15, 0x17, 0x1c);
/// Control group background.
pub const PANEL: Color32 = Color32::from_rgb(0x1d, 0x20, 0x27);
/// Plot background.
pub const PLOT_BG: Color32 = Color32::from_rgb(0x0e, 0x10, 0x14);
pub const PLOT_BORDER: Color32 = Color32::from_rgb(0x2c, 0x31, 0x3a);
pub const GRID: Color32 = Color32::from_rgb(0x1f, 0x23, 0x2a);
pub const GRID_STRONG: Color32 = Color32::from_rgb(0x2b, 0x30, 0x39);
pub const GRID_ZERO: Color32 = Color32::from_rgb(0x55, 0x5c, 0x69);
pub const LABEL: Color32 = Color32::from_rgb(0x6f, 0x77, 0x84);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdc, 0xe3);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x91, 0x9c);
/// Combined EQ curve.
pub const CURVE: Color32 = Color32::from_rgb(0xf2, 0xf4, 0xf7);
/// Spectrum analyzer.
pub const ANALYZER: Color32 = Color32::from_rgb(0x3f, 0xa7, 0xd6);
/// Output group accent.
pub const OUTPUT: Color32 = Color32::from_rgb(0xc5, 0xca, 0xd3);

pub const HPF: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
pub const LOW_SHELF: Color32 = Color32::from_rgb(0xff, 0xa9, 0x4d);
pub const PEAK: Color32 = Color32::from_rgb(0x51, 0xcf, 0x66);
pub const HIGH_SHELF: Color32 = Color32::from_rgb(0xb1, 0x97, 0xfc);

/// Opacity factor for bypassed bands.
pub const BYPASS_DIM: f32 = 0.4;

pub fn band_color(band: Band) -> Color32 {
    match band {
        Band::Hpf => HPF,
        Band::LowShelf => LOW_SHELF,
        Band::Peak => PEAK,
        Band::HighShelf => HIGH_SHELF,
    }
}

/// Band color, dimmed when the band is bypassed.
pub fn band_color_state(band: Band, bypassed: bool) -> Color32 {
    let color = band_color(band);
    if bypassed { dim(color) } else { color }
}

/// Bypassed look: desaturated towards grey and darker (stays opaque).
pub fn dim(color: Color32) -> Color32 {
    let grey = 0x60u8;
    let mix = |c: u8| (c as f32 * BYPASS_DIM + grey as f32 * (1.0 - BYPASS_DIM)).round() as u8;
    Color32::from_rgb(mix(color.r()), mix(color.g()), mix(color.b()))
}

pub fn band_name(band: Band) -> &'static str {
    match band {
        Band::Hpf => "High-Pass",
        Band::LowShelf => "Low Shelf",
        Band::Peak => "Mid Peak",
        Band::HighShelf => "High Shelf",
    }
}

/// One-character node label.
pub fn band_glyph(band: Band) -> &'static str {
    match band {
        Band::Hpf => "1",
        Band::LowShelf => "2",
        Band::Peak => "3",
        Band::HighShelf => "4",
    }
}

/// Install the dark theme. Called once per editor open.
pub fn apply(ctx: &Context) {
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
        let mut visuals = Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = PANEL;
        visuals.extreme_bg_color = PLOT_BG;
        visuals.faint_bg_color = PANEL;
        visuals.override_text_color = None;
        visuals.selection.bg_fill = Color32::from_rgb(0x3a, 0x6e, 0xa5);
        visuals.selection.stroke = Stroke::new(1.0, TEXT);

        let widget_bg = Color32::from_rgb(0x2a, 0x2e, 0x37);
        let widget_bg_hover = Color32::from_rgb(0x34, 0x39, 0x44);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, PLOT_BORDER);
        visuals.widgets.inactive.bg_fill = widget_bg;
        visuals.widgets.inactive.weak_bg_fill = widget_bg;
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, widget_bg);
        visuals.widgets.hovered.bg_fill = widget_bg_hover;
        visuals.widgets.hovered.weak_bg_fill = widget_bg_hover;
        visuals.widgets.active.bg_fill = Color32::from_rgb(0x40, 0x46, 0x52);
        visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x40, 0x46, 0x52);
        for widget in [
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
        ] {
            widget.corner_radius = CornerRadius::same(3);
        }
        style.visuals = visuals;

        style.spacing.item_spacing = vec2(6.0, 4.0);
        style.spacing.button_padding = vec2(6.0, 2.0);
        style.spacing.interact_size.y = 20.0;
    });
}
