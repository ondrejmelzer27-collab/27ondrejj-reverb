//! egui editor. Owned by the GUI.
//!
//! Contract with `lib.rs` (keep these two items stable):
//! - `pub type VocalEqEditor` — the concrete `Plugin::Editor` type.
//! - `pub fn create(params, analyzer, repaint) -> Option<VocalEqEditor>`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use nice_plug::context::gui::GuiContext;
use nice_plug::editor::dpi::LogicalSize;
use nice_plug::prelude::*;
use nice_plug_egui::resizable_window::ResizableWindow;
use nice_plug_egui::widgets::ParamSlider;
use nice_plug_egui::{
    EguiEditor, EguiEditorState, EguiNiceSettings, Frame, NiceEguiApp, RepaintNotifier, baseview,
    create_egui_editor,
};

use crate::analyzer_bus::AnalyzerReceiver;
use crate::params::{MIN_EDITOR_SIZE, VocalEqParams};

/// The plugin's editor type.
pub type VocalEqEditor = EguiEditor<EqApp>;

/// Build the editor. Called once by the wrapper right after the plugin is constructed.
pub fn create(
    params: Arc<VocalEqParams>,
    analyzer: AnalyzerReceiver,
    repaint: RepaintNotifier,
) -> Option<VocalEqEditor> {
    let size = LogicalSize::new(
        params.editor_width.load(Ordering::Relaxed),
        params.editor_height.load(Ordering::Relaxed),
    );
    create_egui_editor(
        EguiEditorState::from_size(size, 1.0),
        repaint,
        EguiNiceSettings::new()
            .with_tile("Vocal EQ")
            .with_resize_hint(
                ResizeHint::RESIZABLE
                    .with_min_logical_size(LogicalSize::new(MIN_EDITOR_SIZE.0, MIN_EDITOR_SIZE.1)),
            ),
        EqApp {
            params,
            analyzer,
            gui: None,
        },
    )
}

pub struct EqApp {
    params: Arc<VocalEqParams>,
    #[allow(dead_code)]
    analyzer: AnalyzerReceiver,
    gui: Option<GuiContext>,
}

impl NiceEguiApp for EqApp {
    fn build(
        &mut self,
        _egui_ctx: egui::Context,
        gui: GuiContext,
        _frame: &mut Frame,
    ) -> Result<(), baseview::HandlerError> {
        self.gui = Some(gui);
        Ok(())
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        let Some(gui) = self.gui.as_ref() else { return };
        let setter = gui.param_setter();
        let params = &self.params;
        ResizableWindow::new("vocal-eq")
            .min_size(egui::vec2(MIN_EDITOR_SIZE.0, MIN_EDITOR_SIZE.1))
            .show(ui, |ui| {
                ui.heading("Vocal EQ");
                ui.label("Output");
                ui.add(ParamSlider::for_param(&params.output_gain, &setter));
            });
    }
}
