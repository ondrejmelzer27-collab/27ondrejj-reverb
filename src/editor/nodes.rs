//! Draggable band nodes on the EQ plot.
//!
//! - drag: frequency (x) and gain (y); the HPF node only moves horizontally
//! - mouse wheel over a node: Q (no-op for the HPF)
//! - double-click: toggle the band's bypass
//!
//! Every parameter change is wrapped in `begin_set_parameter` / `end_set_parameter` so hosts
//! record automation and undo correctly.

use egui::{Event, MouseWheelUnit, Pos2, Response, Ui, Vec2};
use nice_plug::prelude::*;

use super::plot::PlotMapping;
use crate::dsp::Band;
use crate::params::{BandParams, VocalEqParams};

/// Drawn node radius in points.
pub const NODE_RADIUS: f32 = 7.0;
/// Pointer distance within which a node counts as hit.
pub const NODE_HIT_RADIUS: f32 = 12.0;
/// Q multiplier per mouse-wheel notch.
const Q_PER_NOTCH: f32 = 1.12;
/// Points of smooth (trackpad) scrolling that count as one wheel notch.
const POINTS_PER_NOTCH: f32 = 40.0;

/// A node's on-screen state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeView {
    pub band: Band,
    pub pos: Pos2,
    pub bypassed: bool,
}

/// Shelf/peak parameters of `band` (`None` for the HPF).
pub fn band_params(params: &VocalEqParams, band: Band) -> Option<&BandParams> {
    match band {
        Band::Hpf => None,
        Band::LowShelf => Some(&params.low_shelf),
        Band::Peak => Some(&params.peak),
        Band::HighShelf => Some(&params.high_shelf),
    }
}

pub fn freq_param(params: &VocalEqParams, band: Band) -> &FloatParam {
    match band_params(params, band) {
        Some(p) => &p.freq,
        None => &params.hpf.freq,
    }
}

pub fn bypass_param(params: &VocalEqParams, band: Band) -> &BoolParam {
    match band_params(params, band) {
        Some(p) => &p.bypass,
        None => &params.hpf.bypass,
    }
}

/// Plain-value bounds of a float parameter.
pub fn param_bounds(param: &FloatParam) -> (f32, f32) {
    let range = param.range();
    let a = range.unnormalize(0.0);
    let b = range.unnormalize(1.0);
    (a.min(b), a.max(b))
}

/// Set a float parameter to `value` (clamped to its range, snapped to its step size).
/// Must be called between `begin_set_parameter` and `end_set_parameter`. Skips the call if
/// the value would not change.
pub fn set_float(setter: &ParamSetter, param: &FloatParam, value: f32) {
    if !value.is_finite() {
        return;
    }
    let (lo, hi) = param_bounds(param);
    let snapped = param.preview_plain(param.preview_normalized(value.clamp(lo, hi)));
    if snapped != param.unmodulated_plain_value() {
        setter.set_parameter(param, snapped);
    }
}

/// Toggle a bool parameter as one complete gesture.
pub fn toggle_bool(setter: &ParamSetter, param: &BoolParam) {
    let new_value = !param.unmodulated_plain_value();
    setter.begin_set_parameter(param);
    setter.set_parameter(param, new_value);
    setter.end_set_parameter(param);
}

/// Node positions for the current parameter values. The HPF node sits on the 0 dB line.
pub fn node_views(params: &VocalEqParams, mapping: &PlotMapping) -> [NodeView; 4] {
    Band::ALL.map(|band| {
        let freq = freq_param(params, band).value();
        let gain = band_params(params, band).map_or(0.0, |p| p.gain.value());
        NodeView {
            band,
            pos: mapping.pos(freq, gain),
            bypassed: bypass_param(params, band).value(),
        }
    })
}

/// The node under `pointer` within `radius`; the nearest wins, ties go to the node drawn
/// last (later in `nodes`).
pub fn hit_test(nodes: &[NodeView], pointer: Pos2, radius: f32) -> Option<Band> {
    let mut best = None;
    let mut best_dist_sq = radius * radius;
    for node in nodes {
        let dist_sq = (node.pos - pointer).length_sq();
        if dist_sq <= best_dist_sq {
            best_dist_sq = dist_sq;
            best = Some(node.band);
        }
    }
    best
}

/// Frequency and gain for a node dragged to `target`, clamped to the given bounds.
pub fn drag_values(
    mapping: &PlotMapping,
    target: Pos2,
    freq_bounds: (f32, f32),
    gain_bounds: (f32, f32),
) -> (f32, f32) {
    let freq = mapping
        .x_to_freq(target.x)
        .clamp(freq_bounds.0, freq_bounds.1);
    let gain = mapping
        .y_to_db(target.y)
        .clamp(gain_bounds.0, gain_bounds.1);
    (freq, gain)
}

/// Mouse-wheel notches (positive = wheel up) in this frame's events.
pub fn wheel_notches(events: &[Event]) -> f32 {
    events
        .iter()
        .map(|event| match event {
            Event::MouseWheel { unit, delta, .. } => match unit {
                MouseWheelUnit::Line => delta.y,
                MouseWheelUnit::Point => delta.y / POINTS_PER_NOTCH,
                MouseWheelUnit::Page => delta.y * 4.0,
            },
            _ => 0.0,
        })
        .sum()
}

/// New Q after `notches` wheel notches.
pub fn scrolled_q(q: f32, notches: f32) -> f32 {
    q * Q_PER_NOTCH.powf(notches)
}

#[derive(Debug, Clone, Copy)]
struct DragState {
    band: Band,
    /// Node position minus the press position, so the node does not jump to the pointer.
    grab_offset: Vec2,
}

/// Pointer interaction state for the nodes.
#[derive(Debug, Default)]
pub struct NodeInteraction {
    drag: Option<DragState>,
    hovered: Option<Band>,
}

impl NodeInteraction {
    /// The node being dragged, else the hovered one.
    pub fn focus(&self) -> Option<Band> {
        self.drag.map(|d| d.band).or(self.hovered)
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Handle this frame's input on the plot `response` (which must sense click + drag).
    pub fn handle(
        &mut self,
        ui: &Ui,
        response: &Response,
        mapping: &PlotMapping,
        params: &VocalEqParams,
        setter: &ParamSetter,
        nodes: &[NodeView],
    ) {
        self.hovered = response
            .hover_pos()
            .and_then(|p| hit_test(nodes, p, NODE_HIT_RADIUS));

        // Start dragging the node under the press position.
        if response.drag_started() && self.drag.is_none() {
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .or_else(|| response.interact_pointer_pos());
            if let Some(origin) = origin
                && let Some(node) = hit_test(nodes, origin, NODE_HIT_RADIUS)
                    .and_then(|band| nodes.iter().find(|n| n.band == band))
            {
                begin_drag(setter, params, node.band);
                self.drag = Some(DragState {
                    band: node.band,
                    grab_offset: node.pos - origin,
                });
            }
        }

        if let Some(drag) = self.drag {
            if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    apply_drag(
                        setter,
                        params,
                        mapping,
                        drag.band,
                        pointer + drag.grab_offset,
                    );
                }
            } else {
                end_drag(setter, params, drag.band);
                self.drag = None;
            }
        }

        if response.double_clicked()
            && let Some(pointer) = response.interact_pointer_pos()
            && let Some(band) = hit_test(nodes, pointer, NODE_HIT_RADIUS)
        {
            toggle_bool(setter, bypass_param(params, band));
        }

        // Mouse wheel over (or while dragging) a node adjusts its Q.
        if response.contains_pointer()
            && let Some(band) = self.focus()
            && let Some(band_params) = band_params(params, band)
        {
            let notches = ui.input(|i| wheel_notches(&i.events));
            if notches != 0.0 {
                let q = &band_params.q;
                setter.begin_set_parameter(q);
                set_float(setter, q, scrolled_q(q.unmodulated_plain_value(), notches));
                setter.end_set_parameter(q);
            }
        }
    }

    /// End any gesture still in progress (e.g. the window closed mid-drag).
    pub fn cancel(&mut self, params: &VocalEqParams, setter: &ParamSetter) {
        if let Some(drag) = self.drag.take() {
            end_drag(setter, params, drag.band);
        }
        self.hovered = None;
    }
}

fn begin_drag(setter: &ParamSetter, params: &VocalEqParams, band: Band) {
    setter.begin_set_parameter(freq_param(params, band));
    if let Some(p) = band_params(params, band) {
        setter.begin_set_parameter(&p.gain);
    }
}

fn apply_drag(
    setter: &ParamSetter,
    params: &VocalEqParams,
    mapping: &PlotMapping,
    band: Band,
    target: Pos2,
) {
    let freq = freq_param(params, band);
    match band_params(params, band) {
        Some(p) => {
            let (f, g) = drag_values(mapping, target, param_bounds(freq), param_bounds(&p.gain));
            set_float(setter, freq, f);
            set_float(setter, &p.gain, g);
        }
        None => {
            let (f, _) = drag_values(mapping, target, param_bounds(freq), (0.0, 0.0));
            set_float(setter, freq, f);
        }
    }
}

fn end_drag(setter: &ParamSetter, params: &VocalEqParams, band: Band) {
    setter.end_set_parameter(freq_param(params, band));
    if let Some(p) = band_params(params, band) {
        setter.end_set_parameter(&p.gain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::test_util::{Frames, Gesture, MockGui, button, moved, plot_rect};
    use crate::params::BAND_GAIN_RANGE_DB;
    use egui::{Rect, pos2, vec2};

    fn mapping() -> PlotMapping {
        PlotMapping::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 480.0)))
    }

    fn node(band: Band, x: f32, y: f32) -> NodeView {
        NodeView {
            band,
            pos: pos2(x, y),
            bypassed: false,
        }
    }

    #[test]
    fn hit_test_picks_nearest_within_radius() {
        let nodes = [
            node(Band::Hpf, 100.0, 240.0),
            node(Band::LowShelf, 300.0, 200.0),
            node(Band::Peak, 310.0, 205.0),
        ];
        assert_eq!(hit_test(&nodes, pos2(102.0, 243.0), 12.0), Some(Band::Hpf));
        assert_eq!(
            hit_test(&nodes, pos2(301.0, 200.0), 12.0),
            Some(Band::LowShelf)
        );
        assert_eq!(hit_test(&nodes, pos2(309.0, 205.0), 12.0), Some(Band::Peak));
        assert_eq!(hit_test(&nodes, pos2(500.0, 240.0), 12.0), None);
        // Just outside / just inside the radius.
        assert_eq!(hit_test(&nodes, pos2(100.0, 252.5), 12.0), None);
        assert_eq!(hit_test(&nodes, pos2(100.0, 251.5), 12.0), Some(Band::Hpf));
    }

    #[test]
    fn hit_test_ties_go_to_topmost() {
        let nodes = [
            node(Band::LowShelf, 200.0, 200.0),
            node(Band::Peak, 200.0, 200.0),
        ];
        assert_eq!(hit_test(&nodes, pos2(201.0, 200.0), 12.0), Some(Band::Peak));
    }

    #[test]
    fn node_views_follow_params() {
        let params = VocalEqParams::default();
        let m = mapping();
        let nodes = node_views(&params, &m);
        assert_eq!(nodes.map(|n| n.band), Band::ALL);
        // Defaults: all gains 0 dB, HPF bypassed.
        for n in &nodes {
            assert!((n.pos.y - m.db_to_y(0.0)).abs() < 1e-3);
        }
        assert!(nodes[0].bypassed);
        assert!((m.x_to_freq(nodes[0].pos.x) - 80.0).abs() < 0.01);
        assert!((m.x_to_freq(nodes[2].pos.x) - 1_000.0).abs() < 0.1);
        // Every node is hit at its own position.
        for n in &nodes {
            assert_eq!(hit_test(&nodes, n.pos, NODE_HIT_RADIUS), Some(n.band));
        }
    }

    #[test]
    fn drag_values_clamp_to_param_ranges() {
        let params = VocalEqParams::default();
        let m = mapping();
        let freq_bounds = param_bounds(&params.peak.freq);
        let gain_bounds = param_bounds(&params.peak.gain);
        assert_eq!(freq_bounds, (100.0, 12_000.0));
        assert_eq!(gain_bounds, (-BAND_GAIN_RANGE_DB, BAND_GAIN_RANGE_DB));

        let (f, g) = drag_values(&m, m.pos(2_000.0, 6.0), freq_bounds, gain_bounds);
        assert!((f - 2_000.0).abs() < 0.5 && (g - 6.0).abs() < 1e-3);

        // Way outside the plot: clamped.
        let (f, g) = drag_values(&m, pos2(-500.0, -500.0), freq_bounds, gain_bounds);
        assert_eq!((f, g), (100.0, BAND_GAIN_RANGE_DB));
        let (f, g) = drag_values(&m, pos2(5_000.0, 5_000.0), freq_bounds, gain_bounds);
        assert_eq!((f, g), (12_000.0, -BAND_GAIN_RANGE_DB));
    }

    /// Run one frame of node interaction on the test plot.
    fn interact(
        frames: &mut Frames,
        interaction: &mut NodeInteraction,
        params: &VocalEqParams,
        setter: &ParamSetter,
        events: Vec<Event>,
    ) {
        frames.run(events, |ui| {
            let rect = plot_rect();
            let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
            let mapping = PlotMapping::new(rect);
            let nodes = node_views(params, &mapping);
            interaction.handle(ui, &response, &mapping, params, setter, &nodes);
        });
    }

    #[test]
    fn dragging_a_node_sets_freq_and_gain_in_one_gesture() {
        let params = VocalEqParams::default();
        let (mock, gui) = MockGui::context();
        let setter = gui.param_setter();
        let mut frames = Frames::new(1000.0, 600.0);
        let mut interaction = NodeInteraction::default();
        let m = PlotMapping::new(plot_rect());

        // Grab the peak node slightly off-centre; the grab offset must be preserved.
        let node = m.pos(1_000.0, 0.0);
        let press = node + vec2(3.0, 2.0);
        let target = press + vec2(80.0, -60.0);
        let run = |frames: &mut Frames, interaction: &mut NodeInteraction, events| {
            interact(frames, interaction, &params, &setter, events)
        };
        // egui hit-tests against the previous frame's widgets: hover needs a second frame.
        run(&mut frames, &mut interaction, vec![moved(press)]);
        run(&mut frames, &mut interaction, vec![]);
        assert_eq!(interaction.focus(), Some(Band::Peak));
        run(&mut frames, &mut interaction, vec![button(press, true)]);
        for step in 1..=4 {
            let pos = press + (target - press) * (step as f32 / 4.0);
            run(&mut frames, &mut interaction, vec![moved(pos)]);
        }
        assert!(interaction.is_dragging());
        run(&mut frames, &mut interaction, vec![button(target, false)]);
        run(&mut frames, &mut interaction, vec![]);
        assert!(!interaction.is_dragging());

        let gestures = mock.take();
        let freq = params.peak.freq.as_ptr();
        let gain = params.peak.gain.as_ptr();
        assert_eq!(
            &gestures[..2],
            &[Gesture::Begin(freq), Gesture::Begin(gain)]
        );
        assert_eq!(
            &gestures[gestures.len() - 2..],
            &[Gesture::End(freq), Gesture::End(gain)]
        );
        let last_set = |ptr| {
            gestures.iter().rev().find_map(|g| match *g {
                Gesture::Set(p, value) if p == ptr => Some(value),
                _ => None,
            })
        };
        let expected = node + (target - press);
        let want_freq = params.peak.freq.preview_normalized(m.x_to_freq(expected.x));
        let want_gain = params.peak.gain.preview_normalized(m.y_to_db(expected.y));
        assert!((last_set(freq).unwrap() - want_freq).abs() < 1e-3);
        assert!((last_set(gain).unwrap() - want_gain).abs() < 2e-3);
        assert!(gestures.iter().all(
            |g| matches!(g, Gesture::Begin(p) | Gesture::Set(p, _) | Gesture::End(p)
                    if *p == freq || *p == gain)
        ));
    }

    #[test]
    fn dragging_the_hpf_only_changes_frequency() {
        let params = VocalEqParams::default();
        let (mock, gui) = MockGui::context();
        let setter = gui.param_setter();
        let mut frames = Frames::new(1000.0, 600.0);
        let mut interaction = NodeInteraction::default();
        let m = PlotMapping::new(plot_rect());

        let press = m.pos(80.0, 0.0);
        let target = press + vec2(40.0, 100.0);
        for events in [
            vec![moved(press)],
            vec![button(press, true)],
            vec![moved(press + vec2(20.0, 50.0))],
            vec![moved(target)],
            vec![button(target, false)],
        ] {
            interact(&mut frames, &mut interaction, &params, &setter, events);
        }
        let freq = params.hpf.freq.as_ptr();
        let gestures = mock.take();
        assert_eq!(gestures.first(), Some(&Gesture::Begin(freq)));
        assert_eq!(gestures.last(), Some(&Gesture::End(freq)));
        assert!(gestures.iter().all(|g| matches!(g,
            Gesture::Begin(p) | Gesture::Set(p, _) | Gesture::End(p) if *p == freq)));
    }

    #[test]
    fn double_click_toggles_bypass() {
        let params = VocalEqParams::default();
        let (mock, gui) = MockGui::context();
        let setter = gui.param_setter();
        let mut frames = Frames::new(1000.0, 600.0);
        let mut interaction = NodeInteraction::default();
        let m = PlotMapping::new(plot_rect());

        let pos = m.pos(10_000.0, 0.0);
        interact(
            &mut frames,
            &mut interaction,
            &params,
            &setter,
            vec![moved(pos)],
        );
        for _ in 0..2 {
            interact(
                &mut frames,
                &mut interaction,
                &params,
                &setter,
                vec![button(pos, true)],
            );
            interact(
                &mut frames,
                &mut interaction,
                &params,
                &setter,
                vec![button(pos, false)],
            );
        }
        let bypass = params.high_shelf.bypass.as_ptr();
        assert_eq!(
            mock.take(),
            vec![
                Gesture::Begin(bypass),
                Gesture::Set(bypass, 1.0),
                Gesture::End(bypass)
            ]
        );

        // A single click elsewhere does nothing.
        frames.wait(1.0);
        let empty = m.pos(300.0, 12.0);
        for events in [
            vec![moved(empty)],
            vec![button(empty, true)],
            vec![button(empty, false)],
        ] {
            interact(&mut frames, &mut interaction, &params, &setter, events);
        }
        assert!(mock.take().is_empty());
    }

    #[test]
    fn wheel_over_node_changes_q_but_not_for_hpf() {
        let params = VocalEqParams::default();
        let (mock, gui) = MockGui::context();
        let setter = gui.param_setter();
        let mut frames = Frames::new(1000.0, 600.0);
        let mut interaction = NodeInteraction::default();
        let m = PlotMapping::new(plot_rect());
        let wheel_up = Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::NONE,
        };

        let peak = m.pos(1_000.0, 0.0);
        interact(
            &mut frames,
            &mut interaction,
            &params,
            &setter,
            vec![moved(peak)],
        );
        interact(
            &mut frames,
            &mut interaction,
            &params,
            &setter,
            vec![wheel_up.clone()],
        );
        let q = params.peak.q.as_ptr();
        let gestures = mock.take();
        assert_eq!(gestures.len(), 3);
        assert_eq!(gestures[0], Gesture::Begin(q));
        assert_eq!(gestures[2], Gesture::End(q));
        let Gesture::Set(p, value) = gestures[1] else {
            panic!("expected a set, got {gestures:?}");
        };
        assert_eq!(p, q);
        let want = params.peak.q.preview_normalized(Q_PER_NOTCH);
        assert!((value - want).abs() < 1e-4);

        // HPF: no-op.
        let hpf = m.pos(80.0, 0.0);
        interact(
            &mut frames,
            &mut interaction,
            &params,
            &setter,
            vec![moved(hpf)],
        );
        interact(
            &mut frames,
            &mut interaction,
            &params,
            &setter,
            vec![wheel_up],
        );
        assert!(mock.take().is_empty());
    }

    #[test]
    fn wheel_and_q() {
        let wheel = |unit, y| Event::MouseWheel {
            unit,
            delta: vec2(0.0, y),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::NONE,
        };
        let events = [
            wheel(MouseWheelUnit::Line, 1.0),
            Event::PointerGone,
            wheel(MouseWheelUnit::Point, 80.0),
        ];
        assert!((wheel_notches(&events) - 3.0).abs() < 1e-6);
        assert_eq!(wheel_notches(&[]), 0.0);

        assert!(scrolled_q(1.0, 1.0) > 1.0);
        assert!(scrolled_q(1.0, -1.0) < 1.0);
        assert!((scrolled_q(scrolled_q(0.7, 3.0), -3.0) - 0.7).abs() < 1e-5);
    }
}
