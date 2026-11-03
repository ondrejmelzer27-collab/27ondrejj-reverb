//! Headless test helpers: a recording `GuiContextInner` and an egui frame driver.

use std::sync::{Arc, Mutex};

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Ui, pos2, vec2};
use nice_plug::context::gui::GuiContext;
use nice_plug::prelude::*;

/// A parameter gesture as seen by the host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    Begin(ParamPtr),
    Set(ParamPtr, f32),
    End(ParamPtr),
}

/// Records every gesture instead of talking to a host. Values are not applied to the params.
#[derive(Default)]
pub struct MockGui {
    pub gestures: Mutex<Vec<Gesture>>,
}

impl MockGui {
    pub fn context() -> (Arc<Self>, GuiContext) {
        let mock = Arc::new(Self::default());
        let gui = GuiContext::new(mock.clone());
        (mock, gui)
    }

    pub fn take(&self) -> Vec<Gesture> {
        std::mem::take(&mut *self.gestures.lock().unwrap())
    }

    fn push(&self, gesture: Gesture) {
        self.gestures.lock().unwrap().push(gesture);
    }
}

// The trait's methods are `unsafe fn` by signature only; these bodies do nothing unsafe.
impl GuiContextInner for MockGui {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }

    unsafe fn raw_begin_set_parameter(&self, param: ParamPtr) {
        self.push(Gesture::Begin(param));
    }

    unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
        self.push(Gesture::Set(param, normalized));
    }

    unsafe fn raw_end_set_parameter(&self, param: ParamPtr) {
        self.push(Gesture::End(param));
    }

    fn get_state(&self) -> PluginState {
        unimplemented!("not used by the editor")
    }

    fn set_state(&self, _state: PluginState) {
        unimplemented!("not used by the editor")
    }

    fn request_restart(&self) {}
}

/// Drives an `egui::Context` frame by frame with synthetic input.
pub struct Frames {
    pub ctx: egui::Context,
    time: f64,
    screen: Rect,
}

impl Frames {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            ctx: egui::Context::default(),
            time: 0.0,
            screen: Rect::from_min_size(Pos2::ZERO, vec2(width, height)),
        }
    }

    /// Run one frame (1/60 s after the previous one) with `events`.
    pub fn run(&mut self, events: Vec<Event>, mut ui_fn: impl FnMut(&mut Ui)) {
        self.time += 1.0 / 60.0;
        let input = RawInput {
            screen_rect: Some(self.screen),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let mut output = self.ctx.run_ui(input, |ui| ui_fn(ui));
        // No renderer: discard font texture uploads.
        output.textures_delta.clear();
    }

    /// Let time pass without input (e.g. to separate two clicks).
    pub fn wait(&mut self, seconds: f64) {
        self.time += seconds;
    }
}

pub fn moved(pos: Pos2) -> Event {
    Event::PointerMoved(pos)
}

pub fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

pub fn plot_rect() -> Rect {
    Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 480.0))
}
