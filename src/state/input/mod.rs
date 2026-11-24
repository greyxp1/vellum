//! Coordinates device input with editing, focus and cursor feedback.

mod keyboard;
mod pointer;
mod seat;
mod tablet;
mod text;

const CLICK_DURATION_MS: u32 = 300;
const MULTI_CLICK_INTERVAL_MS: u32 = 400;

fn short_click(pressed: Option<u32>, released: Option<u32>) -> bool {
    pressed
        .zip(released)
        .is_some_and(|(pressed, released)| released.wrapping_sub(pressed) <= CLICK_DURATION_MS)
}

pub(super) use keyboard::KeyboardState;
pub(super) use pointer::PointerState;
pub(super) use tablet::TabletState;
pub(super) use text::TextInputState;

use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity;

use super::State;
use crate::OutputId;
use crate::cli::{Visibility, WhiteboardCommand};
use crate::config::{BindingAction, DrawOn};
use crate::draw::{self, Action, Cursor, CursorHint, DrawState, Modifiers, Point, ToolOverride};
use crate::tool::Primitive;

const MAX_PENDING_PEN_SAMPLES: usize = 64;
const PEN_BEND_DEVIATION_SQUARED: f32 = 0.5 * 0.5;

#[derive(Default)]
pub(super) struct PendingPenMotion {
    anchor: Option<Point>,
    samples: Vec<Point>,
    modifiers: Modifiers,
}

impl PendingPenMotion {
    pub(super) fn reset(&mut self, anchor: Option<Point>) {
        self.anchor = anchor;
        self.samples.clear();
    }

    pub(super) fn push(&mut self, point: Point) {
        if self.samples.last() == Some(&point) {
            return;
        }
        if self.samples.len() == MAX_PENDING_PEN_SAMPLES {
            let mut write = 1;
            for read in (2..self.samples.len()).step_by(2) {
                self.samples[write] = self.samples[read];
                write += 1;
            }
            self.samples.truncate(write);
        }
        self.samples.push(point);
    }

    pub(super) fn take(&mut self) -> Option<draw::PenMotion> {
        let &end = self.samples.last()?;
        let bend = self.anchor.and_then(|anchor| {
            self.samples[..self.samples.len() - 1]
                .iter()
                .copied()
                .map(|point| (point, point.segment_distance_squared(anchor, end)))
                .max_by(|(_, first), (_, second)| first.total_cmp(second))
                .filter(|(bend, deviation)| {
                    *deviation >= PEN_BEND_DEVIATION_SQUARED && *bend != anchor && *bend != end
                })
                .map(|(bend, _)| bend)
        });
        self.anchor = Some(end);
        self.samples.clear();
        Some(draw::PenMotion { end, bend })
    }
}

/// Output, position and compositor cursor of one pointing device.
#[derive(Default)]
struct CursorFocus {
    device: Option<WpCursorShapeDeviceV1>,
    output: Option<OutputId>,
    position: Option<Point>,
    serial: Option<u32>,
    shown: Option<Cursor>,
}

impl CursorFocus {
    fn enter(&mut self, output: Option<OutputId>, serial: u32) {
        self.output = output;
        self.serial = Some(serial);
        self.shown = None;
    }

    fn leave(&mut self) {
        self.output = None;
        self.serial = None;
    }

    fn clear(&mut self) {
        self.leave();
        self.position = None;
    }

    fn restore(&mut self) {
        if let (Some(serial), Some(device)) = (self.serial, &self.device) {
            device.set_shape(serial, Shape::Default);
        }
        self.shown = None;
    }

    /// Shows the tool cursor at the device position, returning whether its preview changed.
    fn show(
        &mut self,
        draw: &mut DrawState,
        tool_override: ToolOverride,
        pen_tip: Option<Point>,
        hide: impl FnOnce(u32),
    ) -> Option<bool> {
        let point = self.position?;
        let cursor = draw.editor().cursor(point, tool_override);
        let changed = draw.set_tool_cursor(match cursor.tool() {
            Some(preview) if self.device.is_some() => {
                let pen_tip = pen_tip.filter(|_| preview.tool == Primitive::Pen);
                Some((pen_tip.unwrap_or(point), preview))
            }
            _ => None,
        });
        if let (Some(serial), Some(device)) = (self.serial, &self.device)
            && !self
                .shown
                .is_some_and(|shown| shown.shape() == cursor.shape())
        {
            match cursor.shape() {
                Some(hint) => device.set_shape(serial, cursor_shape(hint)),
                None => hide(serial),
            }
            self.shown = Some(cursor);
        }
        Some(changed)
    }
}

fn cursor_shape(hint: CursorHint) -> Shape {
    match hint {
        CursorHint::Crosshair => Shape::Crosshair,
        CursorHint::Move => Shape::Move,
        CursorHint::NsResize => Shape::NsResize,
        CursorHint::EwResize => Shape::EwResize,
        CursorHint::NwseResize => Shape::NwseResize,
        CursorHint::NeswResize => Shape::NeswResize,
        CursorHint::Text => Shape::Text,
    }
}

impl State {
    fn focus_output(&mut self, output: OutputId) {
        let current = self.draw_on == DrawOn::Current;
        if !self.is_active()
            || !self.wayland.outputs.contains_key(&output)
            || (current
                && self
                    .selected_output
                    .is_some_and(|selected| selected != output))
        {
            return;
        }
        let selection_changed = current && self.selected_output.is_none();
        self.input_output = Some(output);
        if selection_changed {
            self.selected_output = Some(output);
            self.damage_presentation();
            self.freeze_output_selected();
        }
        if self.draw.editor().is_editing_text() || self.pointer.keyboard_grab_active() {
            return;
        }
        if self.keyboard_output != Some(output) || selection_changed {
            self.keyboard_output = Some(output);
            self.update_output_input();
        }
    }

    fn restore_keyboard_focus(&mut self) {
        if self.draw.editor().is_editing_text() {
            return;
        }
        self.focus_keyboard_on_input();
    }

    fn focus_keyboard_on_input(&mut self) {
        if self.pointer.keyboard_grab_active() {
            return;
        }
        let Some(output) = self
            .input_output
            .filter(|output| self.wayland.outputs.contains_key(output))
        else {
            return;
        };
        if self.keyboard_output != Some(output) {
            self.keyboard_output = Some(output);
            self.update_output_input();
        }
    }

    pub(super) fn update_output_input(&mut self) {
        let mut empty_region = None;
        let active = self.is_active() && !self.freeze.defer_input;
        let input_grab_active = self.pointer.input_grab_active() || self.tablet.input_grab_active();
        for (&id, output) in &mut self.wayland.outputs {
            let accepts_input = active
                && match self.draw_on {
                    DrawOn::All => true,
                    DrawOn::Current => {
                        input_grab_active
                            || self.selected_output.is_none_or(|selected| selected == id)
                    }
                };
            let accepts_keyboard = accepts_input && self.keyboard_output == Some(id);
            if output.input_state == (accepts_input, accepts_keyboard) {
                continue;
            }
            output.input_state = (accepts_input, accepts_keyboard);
            output.surface.set_input_region(if accepts_input {
                None
            } else {
                Some(empty_region.get_or_insert_with(|| {
                    self.wayland.compositor.create_region(&self.qhandle, ())
                }))
            });
            output
                .layer_surface
                .set_keyboard_interactivity(if accepts_keyboard {
                    KeyboardInteractivity::Exclusive
                } else {
                    KeyboardInteractivity::None
                });
            // Vulkan owns commits and their FIFO/explicit-sync state. Apply
            // input changes with a buffer instead of issuing an empty commit.
            // An idle/occluded surface may still owe a frame callback, so input
            // changes must be able to bypass normal frame throttling.
            output.commit_pending = true;
            self.draw.damage(id);
        }
        if let Some(empty_region) = empty_region {
            empty_region.destroy();
        }
        self.request_render();
    }

    /// Ends pointer and tablet gestures. Suppression drains held input first.
    pub(super) fn cancel_gestures(&mut self, suppress: bool) {
        self.pointer.cancel_gesture(suppress);
        self.tablet.cancel_gesture(suppress);
    }

    fn interrupt_pointer_gesture(&mut self) {
        let was_grabbed = self.pointer.input_grab_active();
        let position = self.pointer.focus.position;
        self.pointer.cancel_gesture(false);
        if was_grabbed {
            self.finish_interrupted_gesture(position);
            self.update_output_input();
        }
    }

    pub(super) fn finish_interrupted_gesture(&mut self, position: Option<Point>) {
        self.flush_pen_motion();
        let modifiers = self.keyboard.modifiers();
        self.render_requested |= self.draw.finish_pointer_interaction(position, modifiers);
        self.pending_pen_motion.reset(None);
    }

    /// Applies state-level toggles, returning whether the binding was one.
    fn toggle_binding(&mut self, binding: BindingAction) -> bool {
        match binding {
            BindingAction::ToggleWhiteboard => {
                self.whiteboard(WhiteboardCommand::Visibility(Visibility::Toggle));
            }
            BindingAction::TogglePresentation => self.set_presentation(Visibility::Toggle),
            BindingAction::ToggleFreeze => self.toggle_freeze(),
            _ => return false,
        }
        true
    }

    pub(super) fn apply_action(&mut self, action: Action) {
        if self.freeze.capturing() && !matches!(action, Action::Cancel | Action::Clear) {
            return;
        }
        match action {
            Action::Copy => {
                self.copy_text();
                return;
            }
            Action::Cut if !self.copy_text() => return,
            Action::Paste => {
                self.paste_text();
                return;
            }
            _ => {}
        }
        self.flush_pen_motion();
        self.pointer.released_pen_tip = None;
        let clear_on_cancel = self.clear_on_cancel && matches!(action, Action::Cancel);
        let anchor = self.pointer.focus.position;
        let effect = self.draw.handle_action(action, anchor);
        if self.freeze.capturing() {
            self.stop_freeze(false);
        }
        self.render_requested |= effect.changed;
        if effect.deactivate {
            if clear_on_cancel {
                self.clear();
            }
            self.set_mode(super::Mode::Inactive);
        }
        self.restore_keyboard_focus();
        self.refresh_cursor();
    }

    pub(super) fn modifiers_changed(&mut self) {
        self.flush_pen_motion();
        let modifiers = self.keyboard.modifiers();
        self.render_requested |= self.draw.modifiers_changed(modifiers);
        self.refresh_cursor();
    }

    pub(super) fn pointer_down(
        &mut self,
        point: Point,
        modifiers: Modifiers,
        tool_override: ToolOverride,
    ) {
        self.pending_pen_motion.reset(None);
        if self.draw.editor().picker_active() {
            if tool_override != ToolOverride::Eraser {
                self.render_requested |= self.draw.picker_motion(point);
                return;
            }
            self.dismiss_picker();
        }
        self.focus_keyboard_on_input();
        self.render_requested |= self.draw.pointer_down(point, modifiers, tool_override);
        if self.draw.editor().is_drawing_pen() {
            self.pending_pen_motion.reset(Some(point));
        }
    }

    pub(super) fn pointer_motion(&mut self, point: Point, modifiers: Modifiers) {
        if self.draw.editor().picker_active() {
            self.render_requested |= self.draw.picker_motion(point);
            return;
        }
        // Preserve one real bend inside each display frame without letting a
        // high-polling-rate device grow the stroke without bound.
        if self.draw.editor().is_drawing_pen() {
            if self.pending_pen_motion.modifiers != modifiers {
                self.flush_pen_motion();
            }
            self.pending_pen_motion.modifiers = modifiers;
            self.pending_pen_motion.push(point);
            self.request_render();
            return;
        }
        self.render_requested |= self.draw.pointer_motion(point, modifiers);
    }

    pub(super) fn pointer_up(
        &mut self,
        point: Point,
        modifiers: Modifiers,
        latch_picker: bool,
    ) -> Option<Point> {
        if self.freeze.capturing() {
            return None;
        }
        self.flush_pen_motion();
        if self.draw.editor().picker_active() {
            if self.draw.picker_release(point, latch_picker) {
                self.request_render();
                self.restore_keyboard_focus();
            }
            return None;
        }
        let pen_tip = self.draw.editor().pen_release_tip(point, modifiers);
        self.render_requested |= self.draw.pointer_up(point, modifiers);
        self.pending_pen_motion.reset(None);
        pen_tip
    }

    pub(super) fn open_picker(&mut self, point: Point) {
        self.finish_interrupted_gesture(Some(point));
        self.draw.open_picker(point);
        self.request_render();
    }

    fn toggle_picker(&mut self, point: Point) {
        if self.draw.editor().picker_active() {
            self.dismiss_picker();
        } else {
            self.open_picker(point);
        }
    }

    pub(super) fn dismiss_picker(&mut self) {
        self.render_requested |= self.draw.dismiss_picker();
    }

    pub(super) fn text_click_at(&mut self, point: Point, clicks: u8) -> bool {
        let Some(changed) = self.draw.text_click_at(point, clicks) else {
            return false;
        };
        self.focus_keyboard_on_input();
        self.render_requested |= changed;
        true
    }

    pub(super) fn adjust(&mut self, steps: f32, point: Point, modifiers: Modifiers) -> bool {
        let hit_stop = self.draw.adjust(steps, point, modifiers);
        self.request_render();
        hit_stop
    }

    pub(super) fn refresh_cursor(&mut self) {
        let mut shown = None;
        if self.is_active() && !self.pointer.input_grab_active() {
            shown = self.tablet.show_cursor(&mut self.draw);
        }
        if self.is_active()
            && shown.is_none()
            && let Some(pointer) = &self.wayland.pointer
        {
            shown = self.pointer.show_cursor(&mut self.draw, pointer);
        }
        self.render_requested |= shown.unwrap_or_else(|| self.draw.set_tool_cursor(None));
    }

    pub(super) fn flush_pen_motion(&mut self) {
        if let Some(motion) = self.pending_pen_motion.take() {
            let modifiers = self.pending_pen_motion.modifiers;
            self.draw.pen_motion(motion, modifiers);
        }
    }
}
