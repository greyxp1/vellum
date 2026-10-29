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

use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity;

use super::State;
use crate::OutputId;
use crate::config::DrawOn;
use crate::draw::{self, Action, Modifiers, Point};
use crate::tool::Tool;

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

impl State {
    fn focus_output(&mut self, output: OutputId) {
        if !self.is_active() || !self.wayland.outputs.contains_key(&output) {
            return;
        }
        let selection_changed =
            self.draw_on == DrawOn::Current && self.selected_output != Some(output);
        if self.draw_on == DrawOn::Current {
            if let Some(selected) = self.selected_output
                && selected != output
            {
                return;
            }
            self.selected_output = Some(output);
            if selection_changed {
                self.damage_presentation();
            }
        }
        self.input_output = Some(output);
        if selection_changed {
            self.freeze_output_selected();
        }
        if self.draw.is_editing_text() || self.pointer.keyboard_grab_active() {
            return;
        }
        if self.keyboard_output != Some(output) || selection_changed {
            self.keyboard_output = Some(output);
            self.update_output_input();
        }
    }

    fn restore_keyboard_focus(&mut self) {
        if self.draw.is_editing_text() {
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

    fn interrupt_pointer_gesture(&mut self) {
        let was_grabbed = self.pointer.input_grab_active();
        let position = self.pointer.position();
        self.pointer.cancel_gesture();
        if was_grabbed {
            self.finish_interrupted_gesture(position);
            self.update_output_input();
        }
    }

    pub(super) fn finish_interrupted_gesture(&mut self, position: Option<Point>) {
        self.flush_pen_motion();
        if self
            .draw
            .finish_pointer_interaction(position, self.modifiers())
        {
            self.request_render();
        }
        self.pending_pen_motion.reset(None);
    }

    pub(super) fn apply_action(&mut self, action: Action) {
        if self.freeze.capturing() && !matches!(action, Action::Cancel | Action::Clear) {
            return;
        }
        self.flush_pen_motion();
        self.pointer.clear_released_pen_tip();
        let clear_on_cancel = self.clear_on_cancel && matches!(action, Action::Cancel);
        let anchor = self.pointer.position();
        let effect = self.draw.handle_action(action, anchor);
        if self.freeze.capturing() {
            self.stop_freeze(false);
        }
        if effect.changed {
            self.request_render();
        }
        if effect.deactivate {
            if clear_on_cancel {
                self.clear();
            }
            self.deactivate();
        }
        self.restore_keyboard_focus();
        self.refresh_cursor();
    }

    pub(super) fn modifiers(&self) -> Modifiers {
        self.keyboard.modifiers()
    }

    pub(super) fn modifiers_changed(&mut self) {
        self.flush_pen_motion();
        let modifiers = self.modifiers();
        if self.draw.modifiers_changed(modifiers) {
            self.request_render();
        }
        self.refresh_cursor();
    }

    pub(super) fn pointer_down(
        &mut self,
        point: Point,
        modifiers: Modifiers,
        tool_override: draw::ToolOverride,
    ) {
        self.pending_pen_motion.reset(None);
        if self.draw.picker_active() {
            if tool_override == draw::ToolOverride::Eraser {
                self.dismiss_picker();
            } else {
                if self.draw.picker_motion(point) {
                    self.request_render();
                }
                return;
            }
        }
        self.focus_keyboard_on_input();
        let changed = self.draw.pointer_down(point, modifiers, tool_override);
        if self.draw.is_drawing_pen() {
            self.pending_pen_motion.reset(Some(point));
        }
        if changed {
            self.request_render();
        }
    }

    pub(super) fn pointer_motion(&mut self, point: Point, modifiers: Modifiers) {
        if self.draw.picker_active() {
            if self.draw.picker_motion(point) {
                self.request_render();
            }
            return;
        }
        // Preserve one real bend inside each display frame without letting a
        // high-polling-rate device grow the stroke without bound.
        if self.draw.is_drawing_pen() {
            if self.pending_pen_motion.modifiers != modifiers {
                self.flush_pen_motion();
            }
            self.pending_pen_motion.modifiers = modifiers;
            self.pending_pen_motion.push(point);
            self.request_render();
            return;
        }
        if self.draw.pointer_motion(point, modifiers) {
            self.request_render();
        }
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
        if self.draw.picker_active() {
            if self.draw.picker_release(point, latch_picker) {
                self.request_render();
                self.restore_keyboard_focus();
            }
            return None;
        }
        let pen_tip = self.draw.pen_release_tip(point, modifiers);
        if self.draw.pointer_up(point, modifiers) {
            self.request_render();
        }
        self.pending_pen_motion.reset(None);
        pen_tip
    }

    pub(super) fn open_picker(&mut self, point: Point) {
        self.flush_pen_motion();
        self.draw
            .finish_pointer_interaction(Some(point), self.modifiers());
        self.pending_pen_motion.reset(None);
        self.draw.open_picker(point);
        self.request_render();
    }

    fn toggle_picker(&mut self, point: Point) {
        if self.draw.picker_active() {
            self.dismiss_picker();
        } else {
            self.open_picker(point);
        }
    }

    pub(super) fn dismiss_picker(&mut self) {
        if self.draw.dismiss_picker() {
            self.request_render();
        }
    }

    pub(super) fn text_click_at(&mut self, point: Point, clicks: u8) -> bool {
        let Some(changed) = self.draw.text_click_at(point, clicks) else {
            return false;
        };
        self.focus_keyboard_on_input();
        if changed {
            self.request_render();
        }
        true
    }

    pub(super) fn adjust(&mut self, steps: f32, point: Point, modifiers: Modifiers) -> bool {
        let hit_stop = self.draw.adjust(steps, point, modifiers);
        self.request_render();
        hit_stop
    }

    pub(super) fn refresh_cursor(&mut self) {
        if !self.is_active() {
            self.clear_tool_cursor();
            return;
        }
        if !self.pointer.input_grab_active()
            && let Some(preview_changed) = self.tablet.refresh_cursor(&mut self.draw)
        {
            if preview_changed {
                self.request_render();
            }
            return;
        }
        let (Some(point), Some(pointer)) = (self.pointer.position(), &self.wayland.pointer) else {
            self.clear_tool_cursor();
            return;
        };
        let cursor = self.draw.cursor(point, self.pointer.tool_override());
        let preview_changed = self.draw.set_tool_cursor(match cursor.tool() {
            Some(preview) if self.pointer.tool_cursor_supported() => {
                let point = if preview.tool == Tool::Pen {
                    self.pointer.released_pen_tip().unwrap_or(point)
                } else {
                    point
                };
                Some((point, preview))
            }
            _ => None,
        });
        self.pointer.refresh_cursor(pointer, cursor);
        if preview_changed {
            self.request_render();
        }
    }

    fn clear_tool_cursor(&mut self) {
        if self.draw.set_tool_cursor(None) {
            self.request_render();
        }
    }

    pub(super) fn flush_pen_motion(&mut self) {
        if let Some(motion) = self.pending_pen_motion.take() {
            let modifiers = self.pending_pen_motion.modifiers;
            self.draw.pen_motion(motion, modifiers);
        }
    }
}
