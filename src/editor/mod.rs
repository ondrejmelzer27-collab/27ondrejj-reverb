//! egui editor. Owned by the GUI.
//!
//! Contract with `lib.rs` (keep these two items stable):
//! - `pub type VocalEqEditor` — the concrete `Plugin::Editor` type.
//! - `pub fn create(params, analyzer, repaint) -> Option<VocalEqEditor>`.
//!
//! # Editor size persistence
//!
//! `Plugin::editor()` (→ [`create`]) runs once, right after the plugin is constructed and
//! *before* the host restores its state, and nice-plug-egui's `EguiEditorState` cannot be
//! resized from the outside. [`PersistentSizeEditor`] therefore wraps the `EguiEditor`:
//! - the app's `resized()` stores the logical window size in `params.editor_width/height`
//!   (persisted with the plugin state) and remembers the scale factor;
//! - `size()` reports the persisted size while the window is closed, so hosts size their
//!   frame from the restored state;
//! - `spawn()` rebuilds the inner `EguiEditor` with `EguiEditorState::from_size(persisted)`
//!   when the persisted size differs from the inner editor's size, then opens it.

mod controls;
mod nodes;
mod plot;
#[cfg(test)]
mod test_util;
mod theme;
mod view;

use std::error::Error;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use nice_plug::context::gui::GuiContext;
use nice_plug::editor::dpi::{LogicalSize, NativeSize, Size};
use nice_plug::editor::{HostMethods, SpawnedEditor};
use nice_plug::prelude::*;
use nice_plug_egui::resizable_window::ResizableWindow;
use nice_plug_egui::{
    EguiEditor, EguiEditorHandle, EguiEditorState, EguiNiceSettings, Frame, NiceEguiApp,
    RepaintNotifier, baseview, create_egui_editor,
};

use crate::analyzer_bus::AnalyzerReceiver;
use crate::params::{DEFAULT_EDITOR_SIZE, MIN_EDITOR_SIZE, VocalEqParams};
use view::EqView;

/// The plugin's editor type.
pub type VocalEqEditor = PersistentSizeEditor;

/// Largest accepted persisted editor dimension (guards against corrupt state).
const MAX_EDITOR_DIMENSION: f32 = 8192.0;
/// Margin between the window edge and the content.
const OUTER_MARGIN: f32 = 10.0;
/// Gap between the plot and the controls.
const SECTION_GAP: f32 = 8.0;
/// Continuous repaint interval for the analyzer (~60 fps).
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Build the editor. Called once by the wrapper right after the plugin is constructed.
pub fn create(
    params: Arc<VocalEqParams>,
    analyzer: AnalyzerReceiver,
    repaint: RepaintNotifier,
) -> Option<VocalEqEditor> {
    PersistentSizeEditor::new(params, analyzer, repaint)
}

fn resize_hint() -> ResizeHint {
    ResizeHint::RESIZABLE
        .with_min_logical_size(LogicalSize::new(MIN_EDITOR_SIZE.0, MIN_EDITOR_SIZE.1))
}

/// Clamp a persisted dimension to a sane range (`default` if it is not a number).
fn sanitize_dimension(value: f32, min: f32, default: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value.clamp(min, MAX_EDITOR_DIMENSION)
    } else {
        default
    }
}

/// The persisted logical editor size.
fn persisted_size(params: &VocalEqParams) -> (f32, f32) {
    (
        sanitize_dimension(
            params.editor_width.load(Ordering::Relaxed),
            MIN_EDITOR_SIZE.0,
            DEFAULT_EDITOR_SIZE.0,
        ),
        sanitize_dimension(
            params.editor_height.load(Ordering::Relaxed),
            MIN_EDITOR_SIZE.1,
            DEFAULT_EDITOR_SIZE.1,
        ),
    )
}

/// Last known window scale factor (`f32` bits), shared between the editor and its app.
type SharedScale = Arc<AtomicU32>;

struct InnerEditor {
    state: Arc<EguiEditorState>,
    editor: EguiEditor<EqApp>,
}

/// `EguiEditor` wrapper that persists the window size in the plugin state. See the module
/// docs.
pub struct PersistentSizeEditor {
    params: Arc<VocalEqParams>,
    analyzer: AnalyzerReceiver,
    repaint: RepaintNotifier,
    scale: SharedScale,
    /// Only touched from the host's main thread (never from the audio thread).
    inner: Mutex<InnerEditor>,
}

impl PersistentSizeEditor {
    fn new(
        params: Arc<VocalEqParams>,
        analyzer: AnalyzerReceiver,
        repaint: RepaintNotifier,
    ) -> Option<Self> {
        let scale = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let inner = build_inner(
            &params,
            &analyzer,
            &repaint,
            &scale,
            persisted_size(&params),
        )?;
        Some(Self {
            params,
            analyzer,
            repaint,
            scale,
            inner: Mutex::new(inner),
        })
    }

    fn lock_inner(&self) -> MutexGuard<'_, InnerEditor> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn scale_factor(&self) -> f64 {
        let scale = f32::from_bits(self.scale.load(Ordering::Relaxed));
        if scale.is_finite() && scale > 0.0 {
            f64::from(scale)
        } else {
            1.0
        }
    }
}

fn build_inner(
    params: &Arc<VocalEqParams>,
    analyzer: &AnalyzerReceiver,
    repaint: &RepaintNotifier,
    scale: &SharedScale,
    (width, height): (f32, f32),
) -> Option<InnerEditor> {
    let state = EguiEditorState::from_size(LogicalSize::new(width, height), 1.0);
    let editor = create_egui_editor(
        state.clone(),
        repaint.clone(),
        EguiNiceSettings::new()
            .with_tile("Vocal EQ")
            .with_resize_hint(resize_hint()),
        EqApp::new(params.clone(), analyzer.clone(), scale.clone()),
    )?;
    Some(InnerEditor { state, editor })
}

impl Editor for PersistentSizeEditor {
    type Handle = EguiEditorHandle;

    fn spawn(
        &self,
        parent: Option<ParentWindowHandle>,
        wait_for_parent: bool,
        fallback_scale_factor: Option<f64>,
        gui_context: GuiContext,
        host: Option<HostMethods>,
    ) -> Result<SpawnedEditor<Self::Handle>, Box<dyn Error>> {
        if let Some(scale) = fallback_scale_factor.filter(|s| s.is_finite() && *s > 0.0) {
            self.scale
                .store((scale as f32).to_bits(), Ordering::Relaxed);
        }

        let mut inner = self.lock_inner();
        let (width, height) = persisted_size(&self.params);
        let current = inner.state.logical_size();
        let size_differs =
            (current.width - width).abs() > 0.5 || (current.height - height).abs() > 0.5;
        // The inner state's size can't be changed from outside: rebuild it at the persisted
        // size. If that fails, open at the previous size instead.
        if size_differs
            && let Some(rebuilt) = build_inner(
                &self.params,
                &self.analyzer,
                &self.repaint,
                &self.scale,
                (width, height),
            )
        {
            *inner = rebuilt;
        }
        inner.editor.spawn(
            parent,
            wait_for_parent,
            fallback_scale_factor,
            gui_context,
            host,
        )
    }

    fn size(&self) -> NativeSize<u32> {
        let inner = self.lock_inner();
        // While open the window's actual size is authoritative (it may differ from a state
        // the host restored meanwhile). The scale factor is tracked here rather than taken
        // from the inner state, which forgets it whenever it is rebuilt.
        let (width, height) = if inner.state.is_open() {
            let size = inner.state.logical_size();
            (size.width, size.height)
        } else {
            persisted_size(&self.params)
        };
        NativeSize::from_size(
            Size::Logical(LogicalSize::new(f64::from(width), f64::from(height))),
            self.scale_factor(),
        )
    }

    fn resize_hint(&self) -> ResizeHint {
        resize_hint()
    }

    fn track_info_updated(&self, info: TrackInfo) {
        self.lock_inner().editor.track_info_updated(info);
    }

    unsafe fn assume_standalone_in_process(&self) {
        // Safety: forwarded unchanged; the caller upholds `Editor`'s documented invariants.
        unsafe { self.lock_inner().editor.assume_standalone_in_process() }
    }
}

/// The egui application: layout, controls and the EQ display.
pub struct EqApp {
    params: Arc<VocalEqParams>,
    analyzer: AnalyzerReceiver,
    scale: SharedScale,
    gui: Option<GuiContext>,
    view: EqView,
    /// Height of the controls area, measured from the previous frame's layout.
    controls_height: f32,
}

impl EqApp {
    fn new(params: Arc<VocalEqParams>, analyzer: AnalyzerReceiver, scale: SharedScale) -> Self {
        let view = EqView::new(analyzer.sample_rate());
        Self {
            params,
            analyzer,
            scale,
            gui: None,
            view,
            controls_height: controls::DEFAULT_HEIGHT,
        }
    }

    /// A new window opened.
    fn open(&mut self, egui_ctx: &egui::Context, gui: GuiContext) {
        theme::apply(egui_ctx);
        // `editor_closed` is never called: end any gesture left over from a previous window.
        let old_gui = self.gui.replace(gui);
        let old_setter = old_gui.as_ref().map(GuiContext::param_setter);
        self.view.on_open(&self.params, old_setter.as_ref());
    }

    /// One frame.
    fn draw(&mut self, ui: &mut egui::Ui) {
        let Self {
            params,
            analyzer,
            gui,
            view,
            controls_height,
            ..
        } = self;
        let Some(gui) = gui.as_ref() else { return };
        let setter = gui.param_setter();

        view.update_analyzer(ui, analyzer);

        ResizableWindow::new("vocal-eq")
            .min_size(egui::vec2(MIN_EDITOR_SIZE.0, MIN_EDITOR_SIZE.1))
            .show(ui, |ui| {
                let full = ui.max_rect().shrink(OUTER_MARGIN);
                let controls_top = (full.bottom() - *controls_height).max(full.top());
                let controls_rect =
                    egui::Rect::from_min_max(egui::pos2(full.left(), controls_top), full.max);
                let plot_rect = egui::Rect::from_min_max(
                    full.min,
                    egui::pos2(full.right(), (controls_top - SECTION_GAP).max(full.top())),
                );

                let tilt = params.analyzer_tilt.load(Ordering::Relaxed);
                view.show(ui, plot_rect, params, &setter, tilt);

                let needed = controls::show(ui, controls_rect, params, &setter);
                if needed.is_finite() && (needed - *controls_height).abs() > 0.5 {
                    *controls_height = needed.clamp(40.0, 400.0);
                    ui.ctx().request_repaint();
                }
            });

        ui.ctx().request_repaint_after(FRAME_INTERVAL);
    }
}

impl NiceEguiApp for EqApp {
    fn build(
        &mut self,
        egui_ctx: egui::Context,
        gui: GuiContext,
        _frame: &mut Frame,
    ) -> Result<(), baseview::HandlerError> {
        self.open(&egui_ctx, gui);
        Ok(())
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        self.draw(ui);
    }

    fn resized(&mut self, new_size: baseview::WindowSize) {
        let width = new_size.logical.width as f32;
        let height = new_size.logical.height as f32;
        if width.is_finite() && height.is_finite() && width >= 1.0 && height >= 1.0 {
            self.params.editor_width.store(width, Ordering::Relaxed);
            self.params.editor_height.store(height, Ordering::Relaxed);
        }
        let scale = new_size.scale_factor as f32;
        if scale.is_finite() && scale > 0.0 {
            self.scale.store(scale.to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_size_is_sanitized() {
        let params = VocalEqParams::default();
        assert_eq!(persisted_size(&params), DEFAULT_EDITOR_SIZE);

        params.editor_width.store(1234.0, Ordering::Relaxed);
        params.editor_height.store(10.0, Ordering::Relaxed);
        assert_eq!(persisted_size(&params), (1234.0, MIN_EDITOR_SIZE.1));

        params.editor_width.store(f32::NAN, Ordering::Relaxed);
        params.editor_height.store(1e9, Ordering::Relaxed);
        assert_eq!(
            persisted_size(&params),
            (DEFAULT_EDITOR_SIZE.0, MAX_EDITOR_DIMENSION)
        );
    }

    #[test]
    fn size_reports_persisted_size_while_closed() {
        let params = Arc::new(VocalEqParams::default());
        let (_tx, rx) = crate::analyzer_bus::analyzer_bus();
        let editor = create(params.clone(), rx, RepaintNotifier::new()).expect("editor");

        let size = editor.size();
        assert_eq!(
            (size.width, size.height),
            (DEFAULT_EDITOR_SIZE.0 as u32, DEFAULT_EDITOR_SIZE.1 as u32)
        );
        // Host restores state with a different size before opening the editor.
        params.editor_width.store(1100.0, Ordering::Relaxed);
        params.editor_height.store(700.0, Ordering::Relaxed);
        let size = editor.size();
        assert_eq!((size.width, size.height), (1100, 700));
        assert!(editor.resize_hint().can_resize);
    }

    #[test]
    fn resized_persists_logical_size_and_scale() {
        let params = Arc::new(VocalEqParams::default());
        let (_tx, rx) = crate::analyzer_bus::analyzer_bus();
        let editor = create(params.clone(), rx, RepaintNotifier::new()).expect("editor");
        let mut app = EqApp::new(
            params.clone(),
            editor.analyzer.clone(),
            editor.scale.clone(),
        );

        app.resized(baseview::WindowSize {
            physical: baseview::dpi::PhysicalSize::new(2000, 1300),
            logical: baseview::dpi::LogicalSize::new(1000.0, 650.0),
            scale_factor: 2.0,
        });
        assert_eq!(persisted_size(&params), (1000.0, 650.0));
        assert_eq!(editor.scale_factor(), 2.0);
        let size = editor.size();
        if cfg!(target_os = "macos") {
            assert_eq!((size.width, size.height), (1000, 650));
        } else {
            assert_eq!((size.width, size.height), (2000, 1300));
        }
    }

    /// The complete frame (layout, plot, controls), minus the DSP curve evaluation.
    #[test]
    fn full_frames_run_headless() {
        use test_util::{Frames, MockGui, button, moved};

        let params = Arc::new(VocalEqParams::default());
        let (mut tx, rx) = crate::analyzer_bus::analyzer_bus();
        let scale = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let mut app = EqApp::new(params.clone(), rx, scale);
        app.view.skip_dsp_for_tests();
        let (mock, gui) = MockGui::context();

        for (width, height) in [MIN_EDITOR_SIZE, DEFAULT_EDITOR_SIZE] {
            let mut frames = Frames::new(width, height);
            app.open(&frames.ctx, gui.clone());
            let tone: Vec<f32> = (0..1024).map(|n| (n as f32 * 0.05).sin()).collect();
            for frame in 0..30 {
                tx.push_stereo(&tone, &tone);
                let pointer = egui::pos2(width * 0.5, height * 0.3 + frame as f32);
                frames.run(vec![moved(pointer)], |ui| app.draw(ui));
            }
            // The controls' measured height settles and the plot keeps most of the window.
            assert!(
                (60.0..=160.0).contains(&app.controls_height),
                "{}",
                app.controls_height
            );
        }
        assert!(
            mock.take().is_empty(),
            "hovering must not change parameters"
        );

        // The analyzer tilt toggle is the lowest widget of the Output group (bottom right):
        // scan upwards from the group's bottom edge until a click flips it.
        let tilt_before = params.analyzer_tilt.load(Ordering::Relaxed);
        let (width, height) = DEFAULT_EDITOR_SIZE;
        let mut frames = Frames::new(width, height);
        app.open(&frames.ctx, gui.clone());
        let x = width - OUTER_MARGIN - 40.0;
        let mut y = height - OUTER_MARGIN - 2.0;
        let controls_top = height - OUTER_MARGIN - app.controls_height;
        while params.analyzer_tilt.load(Ordering::Relaxed) == tilt_before && y > controls_top {
            let click = egui::pos2(x, y);
            for events in [
                vec![moved(click)],
                vec![],
                vec![button(click, true)],
                vec![button(click, false)],
            ] {
                frames.run(events, |ui| app.draw(ui));
            }
            y -= 4.0;
        }
        assert_ne!(params.analyzer_tilt.load(Ordering::Relaxed), tilt_before);
        // Found below the output gain slider, so no parameter was touched on the way.
        assert!(mock.take().is_empty());
        assert!(y > controls_top + 40.0, "tilt toggle found too high: {y}");
    }
}
