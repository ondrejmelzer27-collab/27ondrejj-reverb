//! The EQ display: grid, spectrum analyzer, EQ curves and band nodes.
//!
//! Curves come from a GUI-owned [`EqChain`] (never the audio thread's): it is created with
//! the analyzer bus' sample rate (recreated if that changes) and updated with
//! [`EqChain::set_params_immediate`] whenever the parameters' target settings change. The
//! per-column magnitudes are cached and only re-evaluated when the settings, sample rate or
//! column count change. All per-column buffers are preallocated for [`MAX_COLUMNS`].

use egui::epaint::{Mesh, PathStroke};
use egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, pos2, vec2,
};
use nice_plug::prelude::*;

use super::nodes::{self, NODE_RADIUS, NodeInteraction, NodeView};
use super::plot::{
    ANALYZER_DB_MAX, ANALYZER_DB_MIN, EQ_DB_RANGE, EQ_DB_STEP, FREQ_LABELS, FREQ_MAX, FREQ_MIN,
    GRID_FREQS, PlotMapping, format_freq, format_gain, is_decade, norm_to_freq,
};
use super::theme;
use crate::analyzer::{MAX_COLUMNS, SpectrumAnalyzer};
use crate::analyzer_bus::AnalyzerReceiver;
use crate::dsp::{Band, EqChain, EqSettings};
use crate::params::{HpfSlopeParam, VocalEqParams};

/// Target horizontal resolution: one curve/analyzer evaluation per this many points.
const POINTS_PER_COLUMN: f32 = 1.5;
/// Curves are clamped to this many dB beyond the visible range (the painter clips them).
const CURVE_DB_LIMIT: f32 = EQ_DB_RANGE + 6.0;

fn band_index(band: Band) -> usize {
    match band {
        Band::Hpf => 0,
        Band::LowShelf => 1,
        Band::Peak => 2,
        Band::HighShelf => 3,
    }
}

/// Number of evaluation columns for a plot `width` points wide.
pub fn column_count(width: f32) -> usize {
    ((width / POINTS_PER_COLUMN).ceil().max(0.0) as usize).clamp(16, MAX_COLUMNS)
}

/// GUI-side state of the EQ display. Lives as long as the editor.
pub struct EqView {
    chain: Option<EqChain>,
    chain_sample_rate: f32,
    last_settings: Option<EqSettings>,
    /// Column count the cached curves were evaluated for (0 = invalid).
    curve_columns: usize,
    /// Frequency of each curve column (log-spaced, inclusive of both axis ends).
    curve_freqs: Vec<f32>,
    combined_db: Vec<f32>,
    band_db: [Vec<f32>; 4],

    analyzer: SpectrumAnalyzer,
    analyzer_db: Vec<f32>,
    last_time: Option<f64>,

    interaction: NodeInteraction,

    /// Tests run against DSP stubs: skip curve evaluation.
    #[cfg(test)]
    skip_dsp: bool,
}

impl EqView {
    #[cfg(test)]
    pub fn skip_dsp_for_tests(&mut self) {
        self.skip_dsp = true;
    }

    pub fn new(sample_rate: f32) -> Self {
        let column_buf = || Vec::with_capacity(MAX_COLUMNS);
        Self {
            chain: None,
            chain_sample_rate: 0.0,
            last_settings: None,
            curve_columns: 0,
            curve_freqs: column_buf(),
            combined_db: column_buf(),
            band_db: [column_buf(), column_buf(), column_buf(), column_buf()],
            analyzer: SpectrumAnalyzer::new(sample_rate),
            analyzer_db: vec![ANALYZER_DB_MIN; MAX_COLUMNS],
            last_time: None,
            interaction: NodeInteraction::default(),
            #[cfg(test)]
            skip_dsp: false,
        }
    }

    /// Called when a new editor window opens: finish stale gestures and restart timing.
    pub fn on_open(&mut self, params: &VocalEqParams, old_setter: Option<&ParamSetter>) {
        if let Some(setter) = old_setter {
            self.interaction.cancel(params, setter);
        }
        self.last_time = None;
        self.analyzer.reset();
    }

    /// Pull new audio from the bus and advance the analyzer's smoothing. Once per frame.
    pub fn update_analyzer(&mut self, ui: &Ui, rx: &AnalyzerReceiver) {
        let now = ui.input(|i| i.time);
        let dt = self.last_time.map_or(0.0, |last| (now - last) as f32);
        self.last_time = Some(now);
        self.analyzer.drain_from(rx);
        self.analyzer.update(dt.clamp(0.0, 0.25));
    }

    /// Draw the plot into `rect` and handle node interaction.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        params: &VocalEqParams,
        setter: &ParamSetter,
        tilt: bool,
    ) {
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let mapping = PlotMapping::new(rect);
        let columns = column_count(rect.width());

        // Interaction first so this frame already draws the new values.
        let nodes = nodes::node_views(params, &mapping);
        self.interaction
            .handle(ui, &response, &mapping, params, setter, &nodes);
        let nodes = nodes::node_views(params, &mapping);
        let settings = params.target_eq_settings();
        self.update_curves(&settings, columns);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(4), theme::PLOT_BG);
        paint_grid(&painter, &mapping);
        self.paint_analyzer(&painter, &mapping, columns, tilt);
        self.paint_curves(&painter, &mapping, &settings);
        paint_labels(&painter, &mapping, tilt);
        self.paint_nodes(&painter, &nodes);
        self.paint_readout(&painter, &mapping, params, &nodes);
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, theme::PLOT_BORDER),
            StrokeKind::Inside,
        );

        self.update_cursor(ui, &response);
    }

    fn update_cursor(&self, ui: &Ui, response: &Response) {
        if self.interaction.is_dragging() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if response.hovered() && self.interaction.focus().is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }

    /// (Re)create the GUI-owned chain and re-evaluate cached curves when needed.
    fn update_curves(&mut self, settings: &EqSettings, columns: usize) {
        #[cfg(test)]
        if self.skip_dsp {
            return;
        }
        let sample_rate = self.analyzer.sample_rate();
        let chain = match &mut self.chain {
            Some(chain) if self.chain_sample_rate == sample_rate => chain,
            slot => {
                self.chain_sample_rate = sample_rate;
                self.last_settings = None;
                slot.insert(EqChain::new(f64::from(sample_rate)))
            }
        };

        let settings_changed = self.last_settings.as_ref() != Some(settings);
        if settings_changed {
            chain.set_params_immediate(settings);
            self.last_settings = Some(*settings);
        }
        if !settings_changed && columns == self.curve_columns {
            return;
        }

        if columns != self.curve_columns {
            self.curve_freqs.clear();
            let last = (columns - 1).max(1) as f32;
            self.curve_freqs
                .extend((0..columns).map(|c| norm_to_freq(c as f32 / last)));
        }
        self.curve_columns = columns;

        // Evaluating above Nyquist is meaningless for a digital filter.
        let max_eval_freq = 0.499 * f64::from(sample_rate);
        let eval = |db: f64| {
            if db.is_finite() {
                (db as f32).clamp(-CURVE_DB_LIMIT, CURVE_DB_LIMIT)
            } else {
                -CURVE_DB_LIMIT
            }
        };
        // All buffers have MAX_COLUMNS capacity: no reallocation here.
        self.combined_db.clear();
        for band in &mut self.band_db {
            band.clear();
        }
        for &freq in &self.curve_freqs {
            let freq = f64::from(freq).min(max_eval_freq);
            self.combined_db.push(eval(chain.magnitude_db(freq)));
            for band in Band::ALL {
                self.band_db[band_index(band)].push(eval(chain.band_magnitude_db(band, freq)));
            }
        }
    }

    fn paint_analyzer(
        &mut self,
        painter: &Painter,
        mapping: &PlotMapping,
        columns: usize,
        tilt: bool,
    ) {
        let values = &mut self.analyzer_db[..columns];
        self.analyzer.map_columns(FREQ_MIN, FREQ_MAX, tilt, values);
        if values.iter().all(|&db| db <= ANALYZER_DB_MIN) {
            return;
        }

        let rect = mapping.rect;
        let column_width = rect.width() / columns as f32;
        // Column centres, with the outermost points stretched to the plot edges.
        let x_of = |c: usize| {
            if c == 0 {
                rect.left()
            } else if c + 1 == columns {
                rect.right()
            } else {
                rect.left() + (c as f32 + 0.5) * column_width
            }
        };
        let fill = strip_mesh(
            values
                .iter()
                .enumerate()
                .map(|(c, &db)| (x_of(c), mapping.analyzer_db_to_y(db))),
            columns,
            rect.bottom(),
            theme::ANALYZER.gamma_multiply(0.42),
            theme::ANALYZER.gamma_multiply(0.06),
        );
        painter.add(Shape::mesh(fill));

        let mut outline = Vec::with_capacity(columns);
        outline.extend(
            values
                .iter()
                .enumerate()
                .map(|(c, &db)| pos2(x_of(c), mapping.analyzer_db_to_y(db))),
        );
        painter.add(Shape::line(
            outline,
            PathStroke::new(1.0, theme::ANALYZER.gamma_multiply(0.75)),
        ));
    }

    fn paint_curves(&self, painter: &Painter, mapping: &PlotMapping, settings: &EqSettings) {
        let columns = self.curve_columns;
        if columns < 2 || self.combined_db.len() != columns {
            return;
        }
        let rect = mapping.rect;
        let x_of = |c: usize| rect.left() + rect.width() * c as f32 / (columns - 1) as f32;
        let points = |values: &[f32]| -> Vec<Pos2> {
            let mut pts = Vec::with_capacity(columns);
            pts.extend(
                values
                    .iter()
                    .enumerate()
                    .map(|(c, &db)| pos2(x_of(c), mapping.db_to_y(db))),
            );
            pts
        };

        // Per-band curves (faint), only for active bands that do something.
        for band in Band::ALL {
            let bypassed = match band {
                Band::Hpf => settings.hpf.bypass,
                Band::LowShelf => settings.low_shelf.bypass,
                Band::Peak => settings.peak.bypass,
                Band::HighShelf => settings.high_shelf.bypass,
            };
            let values = &self.band_db[band_index(band)];
            if bypassed || values.iter().all(|db| db.abs() < 0.01) {
                continue;
            }
            painter.add(Shape::line(
                points(values),
                PathStroke::new(1.25, theme::band_color(band).gamma_multiply(0.55)),
            ));
        }

        // Combined curve: translucent fill to 0 dB plus a prominent line.
        let zero_y = mapping.db_to_y(0.0);
        let fill_color = theme::CURVE.gamma_multiply(0.07);
        let fill = strip_mesh(
            self.combined_db
                .iter()
                .enumerate()
                .map(|(c, &db)| (x_of(c), mapping.db_to_y(db))),
            columns,
            zero_y,
            fill_color,
            fill_color,
        );
        painter.add(Shape::mesh(fill));
        painter.add(Shape::line(
            points(&self.combined_db),
            PathStroke::new(2.0, theme::CURVE),
        ));
    }

    fn paint_nodes(&self, painter: &Painter, nodes: &[NodeView]) {
        let focus = self.interaction.focus();
        for node in nodes {
            let color = theme::band_color_state(node.band, node.bypassed);
            let focused = focus == Some(node.band);
            if focused {
                painter.circle_filled(node.pos, NODE_RADIUS + 5.0, color.gamma_multiply(0.25));
            }
            if node.bypassed {
                painter.circle(
                    node.pos,
                    NODE_RADIUS,
                    theme::PLOT_BG,
                    Stroke::new(1.5, color),
                );
            } else {
                let ring = if focused {
                    theme::CURVE
                } else {
                    theme::PLOT_BG
                };
                painter.circle(node.pos, NODE_RADIUS, color, Stroke::new(1.5, ring));
            }
            let glyph_color = if node.bypassed { color } else { theme::PLOT_BG };
            painter.text(
                node.pos + vec2(0.0, 0.5),
                Align2::CENTER_CENTER,
                theme::band_glyph(node.band),
                FontId::proportional(9.5),
                glyph_color,
            );
        }
    }

    /// Value readout next to the hovered/dragged node.
    fn paint_readout(
        &self,
        painter: &Painter,
        mapping: &PlotMapping,
        params: &VocalEqParams,
        nodes: &[NodeView],
    ) {
        let Some(band) = self.interaction.focus() else {
            return;
        };
        let Some(node) = nodes.iter().find(|n| n.band == band) else {
            return;
        };

        let freq = format_freq(nodes::freq_param(params, band).value());
        let mut text = match nodes::band_params(params, band) {
            Some(p) => format!(
                "{}\n{freq}   {}   Q {:.2}",
                theme::band_name(band),
                format_gain(p.gain.value()),
                p.q.value()
            ),
            None => format!(
                "{}\n{freq}   {}",
                theme::band_name(band),
                slope_label(params.hpf.slope.value())
            ),
        };
        if node.bypassed {
            text.push_str("\nbypassed");
        }

        let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), theme::TEXT);
        let padding = vec2(7.0, 5.0);
        let size = galley.size() + 2.0 * padding;
        let rect = mapping.rect.shrink(4.0);
        // Prefer above-right of the node; flip when it would leave the plot.
        let mut min = node.pos + vec2(NODE_RADIUS + 8.0, -(NODE_RADIUS + 8.0) - size.y);
        if min.x + size.x > rect.right() {
            min.x = node.pos.x - NODE_RADIUS - 8.0 - size.x;
        }
        if min.y < rect.top() {
            min.y = node.pos.y + NODE_RADIUS + 8.0;
        }
        min.x = min
            .x
            .clamp(rect.left(), (rect.right() - size.x).max(rect.left()));
        min.y = min
            .y
            .clamp(rect.top(), (rect.bottom() - size.y).max(rect.top()));
        let box_rect = Rect::from_min_size(min, size);

        painter.rect(
            box_rect,
            CornerRadius::same(4),
            theme::PANEL.gamma_multiply(0.94),
            Stroke::new(1.0, theme::band_color_state(band, node.bypassed)),
            StrokeKind::Inside,
        );
        painter.galley(box_rect.min + padding, galley, theme::TEXT);
    }
}

/// Short label for the HPF slope.
pub fn slope_label(slope: HpfSlopeParam) -> &'static str {
    match slope {
        HpfSlopeParam::Db12 => "12 dB/oct",
        HpfSlopeParam::Db24 => "24 dB/oct",
    }
}

/// Triangle strip between `(x, y)` points and a horizontal `base_y`, with a vertical
/// color gradient from `top` (at the points) to `bottom` (at the base).
fn strip_mesh(
    points: impl Iterator<Item = (f32, f32)>,
    count: usize,
    base_y: f32,
    top: Color32,
    bottom: Color32,
) -> Mesh {
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(2 * count);
    mesh.reserve_triangles(2 * count.saturating_sub(1));
    for (i, (x, y)) in points.enumerate() {
        mesh.colored_vertex(pos2(x, y), top);
        mesh.colored_vertex(pos2(x, base_y), bottom);
        if i > 0 {
            let a = 2 * (i as u32 - 1);
            mesh.add_triangle(a, a + 1, a + 2);
            mesh.add_triangle(a + 1, a + 3, a + 2);
        }
    }
    mesh
}

fn paint_grid(painter: &Painter, mapping: &PlotMapping) {
    let rect = mapping.rect;
    for &freq in &GRID_FREQS {
        let x = mapping.freq_to_x(freq).round() + 0.5;
        let color = if is_decade(freq) {
            theme::GRID_STRONG
        } else {
            theme::GRID
        };
        painter.vline(x, rect.y_range(), Stroke::new(1.0, color));
    }

    let steps = (EQ_DB_RANGE / EQ_DB_STEP) as i32;
    for i in -steps..=steps {
        let db = i as f32 * EQ_DB_STEP;
        let y = mapping.db_to_y(db).round() + 0.5;
        let stroke = if i == 0 {
            Stroke::new(1.0, theme::GRID_ZERO)
        } else {
            Stroke::new(1.0, theme::GRID)
        };
        painter.hline(rect.x_range(), y, stroke);
    }
}

fn paint_labels(painter: &Painter, mapping: &PlotMapping, tilt: bool) {
    let rect = mapping.rect;
    let font = FontId::proportional(10.5);

    for (freq, label) in FREQ_LABELS {
        painter.text(
            pos2(mapping.freq_to_x(freq) + 3.0, rect.bottom() - 3.0),
            Align2::LEFT_BOTTOM,
            label,
            font.clone(),
            theme::LABEL,
        );
    }

    // EQ gain labels on the left (skip the edges).
    let steps = (EQ_DB_RANGE / EQ_DB_STEP) as i32;
    for i in (1 - steps)..steps {
        let db = i as f32 * EQ_DB_STEP;
        let text = if i == 0 {
            "0 dB".to_owned()
        } else {
            format!("{db:+.0}")
        };
        painter.text(
            pos2(rect.left() + 5.0, mapping.db_to_y(db) - 1.0),
            Align2::LEFT_BOTTOM,
            text,
            font.clone(),
            if i == 0 {
                theme::TEXT_DIM
            } else {
                theme::LABEL
            },
        );
    }

    // Analyzer scale on the right, on every other grid line.
    let analyzer_step =
        2.0 * EQ_DB_STEP / (2.0 * EQ_DB_RANGE) * (ANALYZER_DB_MAX - ANALYZER_DB_MIN);
    let mut level = ANALYZER_DB_MAX - analyzer_step;
    while level > ANALYZER_DB_MIN + 1.0 {
        painter.text(
            pos2(rect.right() - 5.0, mapping.analyzer_db_to_y(level) - 1.0),
            Align2::RIGHT_BOTTOM,
            format!("{level:.0}"),
            font.clone(),
            theme::ANALYZER.gamma_multiply(0.55),
        );
        level -= analyzer_step;
    }

    let caption = if tilt {
        "Vocal EQ  ·  analyzer dBFS, tilt 4.5 dB/oct"
    } else {
        "Vocal EQ  ·  analyzer dBFS"
    };
    painter.text(
        pos2(rect.left() + 8.0, rect.top() + 6.0),
        Align2::LEFT_TOP,
        caption,
        FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::test_util::{Frames, MockGui, moved, plot_rect};

    #[test]
    fn column_count_is_bounded() {
        assert_eq!(column_count(0.0), 16);
        assert_eq!(column_count(f32::NAN), 16);
        assert_eq!(column_count(900.0), 600);
        assert_eq!(column_count(1e6), MAX_COLUMNS);
    }

    #[test]
    fn strip_mesh_has_two_triangles_per_segment() {
        let mesh = strip_mesh(
            [(0.0, 1.0), (1.0, 2.0), (2.0, 0.5)].into_iter(),
            3,
            5.0,
            Color32::WHITE,
            Color32::BLACK,
        );
        assert_eq!(mesh.vertices.len(), 6);
        assert_eq!(mesh.indices.len(), 2 * 2 * 3);
        assert!(mesh.is_valid());
    }

    /// Everything except the DSP-backed curve evaluation (fake curve data), including the
    /// hover readout of every node.
    #[test]
    fn painting_runs_headless() {
        let params = VocalEqParams::default();
        let (_mock, gui) = MockGui::context();
        let setter = gui.param_setter();
        let mut view = EqView::new(48_000.0);
        let rect = plot_rect();
        let mapping = PlotMapping::new(rect);
        let columns = column_count(rect.width());

        view.curve_columns = columns;
        view.combined_db.clear();
        view.combined_db
            .extend((0..columns).map(|c| 6.0 * (c as f32 * 0.02).sin()));
        for (i, band) in view.band_db.iter_mut().enumerate() {
            band.clear();
            band.extend((0..columns).map(|c| i as f32 * (c as f32 * 0.01).cos()));
        }
        let tone: Vec<f32> = (0..3 * crate::analyzer::FFT_SIZE)
            .map(|n| (n as f32 * 0.13).sin())
            .collect();
        view.analyzer.push_samples(&tone);
        view.analyzer.update(1.0);

        let mut frames = Frames::new(1000.0, 600.0);
        let settings = params.target_eq_settings();
        let nodes = nodes::node_views(&params, &mapping);
        for (i, node) in nodes.iter().enumerate() {
            let tilt = i % 2 == 0;
            for _ in 0..2 {
                frames.run(vec![moved(node.pos)], |ui| {
                    let response = ui.allocate_rect(rect, Sense::click_and_drag());
                    view.interaction
                        .handle(ui, &response, &mapping, &params, &setter, &nodes);
                    let painter = ui.painter_at(rect);
                    paint_grid(&painter, &mapping);
                    view.paint_analyzer(&painter, &mapping, columns, tilt);
                    view.paint_curves(&painter, &mapping, &settings);
                    paint_labels(&painter, &mapping, tilt);
                    view.paint_nodes(&painter, &nodes);
                    view.paint_readout(&painter, &mapping, &params, &nodes);
                });
            }
            assert_eq!(view.interaction.focus(), Some(node.band));
        }
        assert!(view.analyzer_db[..columns].iter().any(|&db| db > -20.0));
    }
}
