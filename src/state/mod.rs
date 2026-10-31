//! Overlay lifecycle and application state.

mod freeze;
mod input;
mod output;
mod render;
mod wayland;

use color::DynamicColor;
use wayland_client::QueueHandle;

use crate::OutputId;
use crate::cli::{VisibilityCommand, WhiteboardCommand};
use crate::config::DrawOn;
use crate::draw::{self, Action, Canvas, Presentation};
use crate::render::GpuContext;
use input::PendingPenMotion;
use wayland::WaylandState;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Inactive,
    Desktop,
    Whiteboard,
}

impl Mode {
    fn canvas(self) -> Canvas {
        if self == Self::Whiteboard {
            Canvas::Whiteboard
        } else {
            Canvas::Desktop
        }
    }
}

pub(crate) struct State {
    pub fatal_error: Option<String>,
    mode: Mode,
    annotations_visible: bool,
    whiteboard_color: crate::Rgba,
    draw_on: DrawOn,
    selected_output: Option<OutputId>,
    input_output: Option<OutputId>,
    keyboard_output: Option<OutputId>,
    clear_on_cancel: bool,
    freeze: freeze::Freeze,
    pending_pen_motion: PendingPenMotion,
    render_requested: bool,

    wayland: WaylandState,
    draw: draw::DrawState,
    keyboard: input::KeyboardState,
    text_input: input::TextInputState,
    pointer: input::PointerState,
    tablet: input::TabletState,

    gpu: Option<GpuContext>,
    qhandle: QueueHandle<State>,
}

impl State {
    pub(crate) fn can_reload(&self) -> bool {
        !self.pointer.input_grab_active()
            && !self.tablet.input_grab_active()
            && !self.draw.is_editing_text()
            && !self.freeze.capturing()
    }

    pub(crate) fn reload(
        &mut self,
        settings: &crate::config::Settings,
        previous: &crate::config::Settings,
    ) {
        if self.draw_on != settings.draw_on {
            self.stop_freeze(false);
            self.draw_on = settings.draw_on;
            self.selected_output = if self.draw_on == DrawOn::Current {
                self.input_output.or(self.keyboard_output)
            } else {
                None
            };
        }
        if settings.annotations_visible != previous.annotations_visible {
            self.annotations_visible = settings.annotations_visible;
        }
        if settings.whiteboard_color != previous.whiteboard_color {
            self.whiteboard_color = settings.whiteboard_color;
        }
        self.clear_on_cancel = settings.clear_on_cancel;
        self.freeze.on_activate = settings.freeze_on_activate;
        self.keyboard
            .reload(settings.keybinds.clone(), settings.text_keybinds.clone());
        if settings.text_font != previous.text_font {
            crate::text::set_text_font(settings.text_font.clone());
            for output in self.wayland.outputs.values_mut() {
                if let Some(wgpu) = &mut output.wgpu {
                    wgpu.invalidate_text_layouts();
                }
            }
        }
        self.draw.reload(settings, previous);
        self.update_output_input();
        self.damage_presentation();
        self.refresh_cursor();
    }

    pub(crate) fn toggle_input(&mut self) {
        self.set_input_active(!self.is_active());
    }

    pub(crate) fn is_active(&self) -> bool {
        self.mode != Mode::Inactive
    }

    pub(crate) fn is_text_editing(&self) -> bool {
        self.draw.is_editing_text()
    }

    pub(crate) fn is_whiteboard(&self) -> bool {
        self.mode == Mode::Whiteboard
    }

    pub(crate) fn set_input_active(&mut self, active: bool) {
        if !active {
            self.deactivate();
        } else if !self.is_active() {
            self.set_mode(Mode::Desktop);
        }
    }

    fn deactivate(&mut self) {
        self.set_mode(Mode::Inactive);
    }

    fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }
        let was_active = self.is_active();
        self.stop_freeze(true);
        self.pending_pen_motion.reset(None);
        self.keyboard.cancel_repeat();
        if was_active && mode != Mode::Inactive {
            self.pointer.suppress_gesture();
            self.tablet.suppress_gestures();
        } else {
            self.pointer.cancel_gesture();
            self.tablet.cancel_gesture();
        }
        self.draw.deactivate();
        self.sync_text_input();
        self.draw.switch_canvas(mode.canvas());
        self.mode = mode;
        if !was_active || !self.is_active() {
            self.selected_output = None;
        }
        for output in self.wayland.outputs.values_mut() {
            if let Some(wgpu) = &mut output.wgpu {
                wgpu.release_picker_target();
            }
        }
        if self.is_active() {
            if self
                .keyboard_output
                .is_none_or(|id| !self.wayland.outputs.contains_key(&id))
            {
                self.keyboard_output = self.wayland.outputs.keys().next().copied();
            }
        } else {
            self.draw.reset_tools();
            self.pointer.restore_cursor();
            self.tablet.restore_cursors();
        }
        if !was_active && mode == Mode::Desktop && self.freeze.on_activate {
            self.freeze.defer_input = true;
            self.start_freeze();
        }
        self.update_output_input();
        self.damage_presentation();
        self.refresh_cursor();
    }

    pub(crate) fn whiteboard(&mut self, command: WhiteboardCommand) {
        match command {
            WhiteboardCommand::Show => self.set_mode(Mode::Whiteboard),
            WhiteboardCommand::Hide if self.mode == Mode::Whiteboard => {
                self.set_mode(Mode::Desktop)
            }
            WhiteboardCommand::Hide => {}
            WhiteboardCommand::Toggle => self.toggle_whiteboard(),
            WhiteboardCommand::Clear => {
                if self.mode == Mode::Whiteboard {
                    self.clear();
                } else if self.draw.clear_whiteboard() {
                    self.request_render();
                }
            }
            WhiteboardCommand::SetColor { color } => {
                let color = crate::color_to_srgb(color);
                if self.whiteboard_color != color {
                    self.whiteboard_color = color;
                    if self.mode == Mode::Whiteboard {
                        self.damage_presentation();
                    }
                }
            }
        }
    }

    fn toggle_whiteboard(&mut self) {
        self.set_mode(if self.mode == Mode::Whiteboard {
            Mode::Desktop
        } else {
            Mode::Whiteboard
        });
    }

    pub(crate) fn annotations(&mut self, command: VisibilityCommand) {
        let visible = match command {
            VisibilityCommand::Show => true,
            VisibilityCommand::Hide => false,
            VisibilityCommand::Toggle => !self.annotations_visible,
        };
        if self.annotations_visible != visible {
            self.annotations_visible = visible;
            self.damage_presentation();
        }
    }

    fn presentation(&self, output: OutputId) -> Presentation {
        let selected = self.draw_on == DrawOn::All || self.selected_output == Some(output);
        let board = self.mode == Mode::Whiteboard && selected;
        let desktop_drawing = self.mode == Mode::Desktop;
        Presentation {
            canvas: if board {
                Some(Canvas::Whiteboard)
            } else if desktop_drawing || self.annotations_visible {
                Some(Canvas::Desktop)
            } else {
                None
            },
            interactive: self.is_active() && selected,
            background: board.then_some(self.whiteboard_color),
        }
    }

    fn damage_presentation(&mut self) {
        for (&id, output) in &mut self.wayland.outputs {
            output.commit_pending = true;
            self.draw.damage(id);
        }
        self.request_render();
    }

    pub(crate) fn clear(&mut self) {
        self.apply_action(Action::Clear);
    }

    pub(crate) fn set_current_color(&mut self, color: DynamicColor) {
        let rgba = crate::color_to_srgb(color);
        let render = self.draw.set_current_color(rgba);
        if render {
            self.request_render();
        }
        self.refresh_cursor();
    }

    pub(crate) fn next_wakeup(&self) -> Option<std::time::Instant> {
        [
            self.draw.next_wakeup(),
            self.keyboard.next_wakeup(),
            self.freeze.next_wakeup(),
            self.wayland
                .outputs
                .iter()
                .filter(|(id, _)| !self.freeze.hides(**id))
                .filter_map(|(_, output)| output.render_retry)
                .min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn handle_timeouts(&mut self, now: std::time::Instant) {
        self.handle_freeze(now);
        if let Some(action) = self.keyboard.repeat_action(
            now,
            self.draw
                .text_input_snapshot()
                .map(|snapshot| snapshot.session),
        ) {
            self.apply_action(action);
        }
        if self.draw.handle_timeouts(now) {
            self.request_render();
        }
        if self.wayland.outputs.iter().any(|(id, output)| {
            !self.freeze.hides(*id) && output.render_retry.is_some_and(|deadline| now >= deadline)
        }) {
            self.request_render();
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        for output in self.wayland.outputs.values_mut() {
            output.frozen_surface.take();
            output.wgpu.take();
        }
        self.gpu.take();
    }
}
