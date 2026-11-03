//! Parameter controls below the EQ display: one group per band plus output.
//!
//! Sliders are nice-plug-egui `ParamSlider`s (which already wrap drags in begin/end
//! gestures); buttons (bypass, slope) issue complete begin/set/end gestures themselves.

use std::sync::atomic::Ordering;

use egui::{
    Align, Button, Color32, CornerRadius, FontId, Layout, Rect, RichText, Ui, UiBuilder, pos2, vec2,
};
use nice_plug::prelude::*;
use nice_plug_egui::widgets::ParamSlider;

use super::nodes::toggle_bool;
use super::theme;
use crate::dsp::Band;
use crate::params::{BandParams, HpfParams, HpfSlopeParam, VocalEqParams};

/// Horizontal gap between groups.
const GROUP_GAP: f32 = 6.0;
const GROUP_PADDING: f32 = 7.0;
/// Height of the colored strip at the top of each group.
const ACCENT_HEIGHT: f32 = 3.0;
/// Space reserved for the slider's value text.
const VALUE_WIDTH: f32 = 60.0;
const MIN_SLIDER_WIDTH: f32 = 30.0;
/// Initial guess for the controls' height (refined from the measured layout every frame).
pub const DEFAULT_HEIGHT: f32 = 112.0;

/// Draw the control groups into `rect`. Returns the height the tallest group needed, which
/// the caller uses to size `rect` on the next frame.
pub fn show(ui: &mut Ui, rect: Rect, params: &VocalEqParams, setter: &ParamSetter) -> f32 {
    const GROUPS: usize = 5;
    let width = (rect.width() - GROUP_GAP * (GROUPS - 1) as f32) / GROUPS as f32;
    let group_rect = |i: usize| {
        Rect::from_min_size(
            pos2(rect.left() + i as f32 * (width + GROUP_GAP), rect.top()),
            vec2(width, rect.height()),
        )
    };

    let hpf = Group {
        id: "hpf",
        title: theme::band_name(Band::Hpf),
        color: theme::HPF,
        bypass: Some(&params.hpf.bypass),
    }
    .show(ui, group_rect(0), setter, |ui, w| {
        hpf_contents(ui, setter, &params.hpf, w)
    });

    let bands = [
        (Band::LowShelf, &params.low_shelf),
        (Band::Peak, &params.peak),
        (Band::HighShelf, &params.high_shelf),
    ];
    let mut needed = hpf;
    for (i, (band, band_params)) in bands.into_iter().enumerate() {
        let height = Group {
            id: theme::band_name(band),
            title: theme::band_name(band),
            color: theme::band_color(band),
            bypass: Some(&band_params.bypass),
        }
        .show(ui, group_rect(i + 1), setter, |ui, w| {
            band_contents(ui, setter, band_params, w)
        });
        needed = needed.max(height);
    }

    let output = Group {
        id: "output",
        title: "Output",
        color: theme::OUTPUT,
        bypass: None,
    }
    .show(ui, group_rect(4), setter, |ui, w| {
        output_contents(ui, setter, params, w)
    });
    needed.max(output)
}

struct Group<'a> {
    id: &'static str,
    title: &'static str,
    color: Color32,
    bypass: Option<&'a BoolParam>,
}

impl Group<'_> {
    /// Paint the group frame and header, then `add_contents(ui, slider_width)`. Returns the
    /// height the group needed.
    fn show(
        self,
        ui: &mut Ui,
        rect: Rect,
        setter: &ParamSetter,
        add_contents: impl FnOnce(&mut Ui, f32),
    ) -> f32 {
        let bypassed = self.bypass.is_some_and(|p| p.value());
        let accent = if bypassed {
            theme::dim(self.color)
        } else {
            self.color
        };

        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(5), theme::PANEL);
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(rect.width(), ACCENT_HEIGHT)),
            CornerRadius {
                nw: 5,
                ne: 5,
                sw: 0,
                se: 0,
            },
            accent,
        );

        let top_inset = ACCENT_HEIGHT + GROUP_PADDING - 1.0;
        let inner = Rect::from_min_max(
            rect.min + vec2(GROUP_PADDING, top_inset),
            (rect.max - vec2(GROUP_PADDING, GROUP_PADDING)).max(rect.min),
        );
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(self.id)
                .max_rect(inner)
                .layout(Layout::top_down(Align::Min)),
        );
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        // Tint sliders and selected buttons with the band color.
        child.visuals_mut().selection.bg_fill = accent.gamma_multiply(0.6);

        child.horizontal(|ui| {
            let title_color = if bypassed {
                theme::TEXT_DIM
            } else {
                theme::TEXT
            };
            ui.label(RichText::new(self.title).strong().color(title_color));
            if let Some(bypass) = self.bypass {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let text = if bypassed { "OFF" } else { "ON" };
                    let response = ui
                        .add(toggle_button(!bypassed, text))
                        .on_hover_text("Enable / bypass this band (or double-click its node)");
                    if response.clicked() {
                        toggle_bool(setter, bypass);
                    }
                });
            }
        });

        if bypassed {
            child.multiply_opacity(0.5);
        }
        let slider_width = (child.available_width() - VALUE_WIDTH - child.spacing().item_spacing.x)
            .max(MIN_SLIDER_WIDTH);
        add_contents(&mut child, slider_width);

        child.min_rect().height() + top_inset + GROUP_PADDING
    }
}

/// A small toggle button that keeps its frame when not selected.
fn toggle_button(selected: bool, text: &str) -> Button<'_> {
    Button::selectable(selected, RichText::new(text).small()).frame_when_inactive(true)
}

/// A `ParamSlider` with its name drawn inside the bar (abbreviated to the first letter when
/// the bar is too narrow).
fn slider_row<P: Param>(ui: &mut Ui, setter: &ParamSetter, param: &P, label: &str, width: f32) {
    const INSET: f32 = 4.0;
    let response = ui.add(ParamSlider::for_param(param, setter).with_width(width));
    let painter = ui.painter();
    let font = FontId::proportional(10.0);
    let color = theme::TEXT.gamma_multiply(0.75);
    let mut galley = painter.layout_no_wrap(label.to_owned(), font.clone(), color);
    if galley.size().x + 2.0 * INSET > response.rect.width() {
        galley = painter.layout_no_wrap(label.chars().take(1).collect(), font, color);
    }
    let pos = response.rect.left_center() + vec2(INSET, -0.5 * galley.size().y);
    painter.galley(pos, galley, color);
}

fn hpf_contents(ui: &mut Ui, setter: &ParamSetter, hpf: &HpfParams, slider_width: f32) {
    slider_row(ui, setter, &hpf.freq, "FREQ", slider_width);
    ui.horizontal(|ui| {
        let current = hpf.slope.unmodulated_plain_value();
        let button_width = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(0.0);
        for (slope, label) in [
            (HpfSlopeParam::Db12, "12 dB"),
            (HpfSlopeParam::Db24, "24 dB"),
        ] {
            let response = ui
                .add(toggle_button(current == slope, label).min_size(vec2(button_width, 0.0)))
                .on_hover_text("High-pass slope (dB/octave)");
            if response.clicked() && current != slope {
                setter.begin_set_parameter(&hpf.slope);
                setter.set_parameter(&hpf.slope, slope);
                setter.end_set_parameter(&hpf.slope);
            }
        }
    });
}

fn band_contents(ui: &mut Ui, setter: &ParamSetter, band: &BandParams, slider_width: f32) {
    slider_row(ui, setter, &band.freq, "FREQ", slider_width);
    slider_row(ui, setter, &band.gain, "GAIN", slider_width);
    slider_row(ui, setter, &band.q, "Q", slider_width);
}

fn output_contents(ui: &mut Ui, setter: &ParamSetter, params: &VocalEqParams, slider_width: f32) {
    slider_row(ui, setter, &params.output_gain, "GAIN", slider_width);

    let tilt = params.analyzer_tilt.load(Ordering::Relaxed);
    let response = ui
        .add(toggle_button(tilt, "Tilt 4.5 dB/oct").min_size(vec2(ui.available_width(), 0.0)))
        .on_hover_text(
            "Analyzer display tilt: +4.5 dB per octave around 1 kHz, so a typical vocal or \
             mix spectrum reads roughly flat. Display only, does not affect the audio.",
        );
    if response.clicked() {
        params.analyzer_tilt.store(!tilt, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::test_util::{Frames, Gesture, MockGui, button, moved};
    use egui::Pos2;

    fn controls_rect(width: f32) -> Rect {
        Rect::from_min_size(pos2(10.0, 300.0), vec2(width - 20.0, DEFAULT_HEIGHT))
    }

    #[test]
    fn layout_fits_at_min_and_default_width() {
        let params = VocalEqParams::default();
        let (_mock, gui) = MockGui::context();
        let setter = gui.param_setter();
        for width in [crate::params::MIN_EDITOR_SIZE.0, 960.0, 2000.0] {
            let mut frames = Frames::new(width, 600.0);
            let mut needed = 0.0;
            frames.run(vec![], |ui| {
                needed = show(ui, controls_rect(width), &params, &setter);
            });
            assert!(
                (60.0..=160.0).contains(&needed),
                "width {width}: needed {needed}"
            );
        }
    }

    #[test]
    fn bypass_button_toggles_with_a_full_gesture() {
        let params = VocalEqParams::default();
        let (mock, gui) = MockGui::context();
        let setter = gui.param_setter();
        let width = 960.0;
        let mut frames = Frames::new(width, 600.0);
        let rect = controls_rect(width);

        // The ON/OFF button sits at the top-right of the Low Shelf group (second of five).
        let group_width = (rect.width() - 4.0 * GROUP_GAP) / 5.0;
        let group_right = rect.left() + 2.0 * group_width + GROUP_GAP;
        let click: Pos2 = pos2(group_right - GROUP_PADDING - 8.0, rect.top() + 18.0);
        for events in [
            vec![moved(click)],
            vec![],
            vec![button(click, true)],
            vec![button(click, false)],
            vec![],
        ] {
            frames.run(events, |ui| {
                show(ui, rect, &params, &setter);
            });
        }
        let bypass = params.low_shelf.bypass.as_ptr();
        assert_eq!(
            mock.take(),
            vec![
                Gesture::Begin(bypass),
                Gesture::Set(bypass, 1.0),
                Gesture::End(bypass)
            ]
        );
    }
}
