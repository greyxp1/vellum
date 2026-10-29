use std::time::{Duration, Instant};
use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::Proxy;
use wayland_client::QueueHandle;
use wayland_client::WEnum;

use wayland_client::protocol::wl_pointer::WlPointer;

use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;

use super::{MULTI_CLICK_INTERVAL_MS, short_click};
use crate::OutputId;
use crate::config::{BindingAction, MouseButton};
use crate::draw::{Action, Cursor, CursorHint, Modifiers, Point, ToolOverride};
use crate::state::State;

const DRAW_BUTTON: u8 = 1;
const PICKER_BUTTON: u8 = 2;
const ERASER_BUTTON: u8 = 4;
const CLICK_SLOP_SQUARED: f32 = 36.0;
const SCROLL_BURST_TIMEOUT_MS: u32 = 100;
const WHEEL_ACCEL_SAMPLE_COUNT: usize = 3;
const WHEEL_ACCEL_MAX_GAIN: f64 = 8.0;
const WHEEL_ACCEL_CURVE_GAIN: f64 = 11.5;
const WHEEL_ACCEL_CURVE_SCALE: f64 = 12.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum GestureInput {
    Mouse(MouseButton),
    Key(u32),
}

#[derive(Default)]
pub(in crate::state) struct PointerState {
    events: Vec<PointerEvent>,
    event_sequence: EventSequence,
    gesture_bindings: Vec<(GestureInput, u8)>,

    cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    cursor_serial: Option<u32>,
    current_cursor: Option<Cursor>,
    output: Option<OutputId>,
    position: Option<Point>,
    released_pen_tip: Option<Point>,
    suppressed_gesture: bool,
    draw_button_in_picker: bool,
    draw_press_pos: Option<Point>,
    picker_press_time: Option<u32>,
    eraser_press: Option<(u32, Point)>,
    eraser_dragging: bool,
    last_draw_click: Option<(u32, Point, u8)>,
    scroll_remainder: f64,
    wheel_acceleration: WheelAcceleration,
    scroll_stop: Option<ScrollStop>,
}

impl PointerState {
    fn bind_gesture(&mut self, input: GestureInput, mask: Option<u8>, pressed: bool) -> Option<u8> {
        if !pressed {
            let index = self
                .gesture_bindings
                .iter()
                .position(|(held, _)| *held == input)?;
            let (_, mask) = self.gesture_bindings.remove(index);
            return (!self.gesture_bindings.iter().any(|(_, held)| *held == mask)).then_some(mask);
        }
        let mask = mask?;
        if self.gesture_bindings.iter().any(|(held, _)| *held == input) {
            return None;
        }
        let held = self.gesture_bindings.iter().any(|(_, held)| *held == mask);
        self.gesture_bindings.push((input, mask));
        (!held).then_some(mask)
    }

    fn held(&self, mask: u8) -> bool {
        self.gesture_bindings
            .iter()
            .any(|(_, held)| held & mask != 0)
    }

    pub(in crate::state) fn set_cursor_device(
        &mut self,
        cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    ) {
        self.cursor_shape_device = cursor_shape_device;
    }

    pub(in crate::state) fn position(&self) -> Option<Point> {
        self.position
    }

    pub(in crate::state) fn released_pen_tip(&self) -> Option<Point> {
        self.released_pen_tip
    }

    pub(in crate::state) fn clear_released_pen_tip(&mut self) {
        self.released_pen_tip = None;
    }

    pub(in crate::state) fn tool_override(&self) -> ToolOverride {
        if self.held(ERASER_BUTTON) {
            ToolOverride::InvertEraser
        } else {
            ToolOverride::None
        }
    }

    pub(in crate::state) fn tool_cursor_supported(&self) -> bool {
        self.cursor_shape_device.is_some()
    }

    pub(in crate::state) fn input_grab_active(&self) -> bool {
        !self.suppressed_gesture && !self.gesture_bindings.is_empty()
    }

    pub(in crate::state) fn keyboard_grab_active(&self) -> bool {
        !self.suppressed_gesture
            && self
                .gesture_bindings
                .iter()
                .any(|(input, _)| matches!(input, GestureInput::Key(_)))
    }

    pub(in crate::state) fn restore_cursor(&mut self) {
        let (Some(serial), Some(device)) = (self.cursor_serial, &self.cursor_shape_device) else {
            return;
        };
        device.set_shape(serial, Shape::Default);
        self.current_cursor = None;
    }

    pub(in crate::state) fn refresh_cursor(&mut self, pointer: &WlPointer, cursor: Cursor) {
        update_cursor(
            self.cursor_serial,
            self.cursor_shape_device.as_ref(),
            &mut self.current_cursor,
            cursor,
            |serial| pointer.set_cursor(serial, None, 0, 0),
        );
    }

    pub(in crate::state) fn clear_pointer(&mut self) {
        if let Some(device) = self.cursor_shape_device.take() {
            device.destroy();
        }
        *self = Self::default();
    }

    pub(in crate::state) fn remove_output(&mut self, output: OutputId) -> Option<Point> {
        if self.output != Some(output) {
            return None;
        }
        let end_position = self.input_grab_active().then_some(self.position).flatten();
        self.cancel_gesture();
        self.event_sequence = EventSequence::default();
        self.clear_focus();
        end_position
    }

    fn clear_focus(&mut self) {
        self.output = None;
        self.position = None;
        self.released_pen_tip = None;
        self.cursor_serial = None;
    }

    pub(in crate::state) fn cancel_gesture(&mut self) {
        self.gesture_bindings.clear();
        self.released_pen_tip = None;
        self.event_sequence = EventSequence {
            motion: self.event_sequence.motion,
            enter_serial: self.event_sequence.enter_serial,
            left_surface: self.event_sequence.left_surface,
            ..Default::default()
        };
        self.suppressed_gesture = false;
        self.draw_button_in_picker = false;
        self.draw_press_pos = None;
        self.picker_press_time = None;
        self.eraser_press = None;
        self.eraser_dragging = false;
        self.last_draw_click = None;
        self.reset_scroll();
    }

    pub(in crate::state) fn suppress_gesture(&mut self) {
        let bindings = std::mem::take(&mut self.gesture_bindings);
        self.cancel_gesture();
        self.suppressed_gesture = !bindings.is_empty();
        self.gesture_bindings = bindings;
    }

    fn update_state(&mut self, sequence: &EventSequence) {
        if let Some(new_pos) = sequence.motion {
            self.position = Some(new_pos);
        }

        if let Some(serial) = sequence.enter_serial {
            self.cursor_serial = Some(serial);
            self.current_cursor = None;
        }
    }

    fn scroll_steps(&mut self, sequence: &EventSequence, modifiers: Modifiers) -> f32 {
        let scroll_steps = sequence.scroll_steps();
        if scroll_steps != 0.0 && self.scroll_stop_blocks(-scroll_steps, modifiers) {
            if sequence.vertical_axis_stopped {
                self.reset_scroll();
            }
            return 0.0;
        }
        if scroll_steps != 0.0 && !sequence.is_wheel() {
            self.wheel_acceleration.reset();
        }
        self.scroll_remainder -= scroll_steps;
        let steps = self.scroll_remainder.trunc();
        self.scroll_remainder -= steps;
        let steps = if steps != 0.0 && sequence.is_wheel() {
            self.wheel_acceleration
                .apply(sequence.axis_time, steps, modifiers)
        } else {
            steps
        };
        if sequence.vertical_axis_stopped {
            self.reset_scroll();
        }
        steps as f32
    }

    fn scroll_stop_blocks(&mut self, direction: f64, modifiers: Modifiers) -> bool {
        let Some(stop) = &mut self.scroll_stop else {
            return false;
        };
        let modifier_state = (modifiers.ctrl, modifiers.shift);
        let within_burst =
            stop.last_event.elapsed() <= Duration::from_millis(u64::from(SCROLL_BURST_TIMEOUT_MS));
        if stop.positive == direction.is_sign_positive()
            && stop.modifiers == modifier_state
            && within_burst
        {
            stop.last_event = Instant::now();
            true
        } else {
            self.scroll_stop = None;
            false
        }
    }

    fn stop_scroll(&mut self, direction: f32, modifiers: Modifiers) {
        self.scroll_remainder = 0.0;
        self.wheel_acceleration.reset();
        self.scroll_stop = Some(ScrollStop {
            positive: direction.is_sign_positive(),
            last_event: Instant::now(),
            modifiers: (modifiers.ctrl, modifiers.shift),
        });
    }

    fn reset_scroll(&mut self) {
        self.scroll_remainder = 0.0;
        self.wheel_acceleration.reset();
        self.scroll_stop = None;
    }
}

struct ScrollStop {
    positive: bool,
    last_event: Instant,
    modifiers: (bool, bool),
}

#[derive(Default)]
struct WheelAcceleration {
    samples: [(u32, f64); WHEEL_ACCEL_SAMPLE_COUNT],
    sample_count: usize,
    step_remainder: f64,
    modifiers: Option<(bool, bool)>,
}

impl WheelAcceleration {
    fn apply(&mut self, time: Option<u32>, steps: f64, modifiers: Modifiers) -> f64 {
        let Some(time) = time else {
            self.reset();
            return steps;
        };
        let modifier_state = (modifiers.ctrl, modifiers.shift);
        let continues = self.sample_count > 0
            && self.modifiers == Some(modifier_state)
            && self.samples[self.sample_count - 1].1.is_sign_positive() == steps.is_sign_positive()
            && time.wrapping_sub(self.samples[self.sample_count - 1].0) <= SCROLL_BURST_TIMEOUT_MS;
        if !continues {
            self.reset();
            self.modifiers = Some(modifier_state);
        }

        let gain = self.gain(time, steps);
        let accelerated = steps * gain + self.step_remainder;
        let whole_steps = accelerated.trunc();
        self.step_remainder = accelerated - whole_steps;
        self.push_sample(time, steps);
        whole_steps
    }

    fn gain(&self, time: u32, steps: f64) -> f64 {
        if self.sample_count < WHEEL_ACCEL_SAMPLE_COUNT {
            return 1.0;
        }
        let distance = steps.abs()
            + self.samples[1..WHEEL_ACCEL_SAMPLE_COUNT]
                .iter()
                .map(|(_, steps)| steps.abs())
                .sum::<f64>();
        let average_interval_ms = f64::from(time.wrapping_sub(self.samples[0].0)) / distance;
        if average_interval_ms >= f64::from(SCROLL_BURST_TIMEOUT_MS) {
            return 1.0;
        }
        let interval_seconds = average_interval_ms / 1_000.0;
        (WHEEL_ACCEL_CURVE_GAIN * (1.0 + WHEEL_ACCEL_CURVE_SCALE * interval_seconds).powi(-3))
            .clamp(1.0, WHEEL_ACCEL_MAX_GAIN)
    }

    fn push_sample(&mut self, time: u32, steps: f64) {
        if self.sample_count < WHEEL_ACCEL_SAMPLE_COUNT {
            self.samples[self.sample_count] = (time, steps);
            self.sample_count += 1;
        } else {
            self.samples.rotate_left(1);
            self.samples[WHEEL_ACCEL_SAMPLE_COUNT - 1] = (time, steps);
        }
    }

    fn reset(&mut self) {
        self.sample_count = 0;
        self.step_remainder = 0.0;
        self.modifiers = None;
    }
}

struct PointerEvent {
    event: <WlPointer as Proxy>::Event,
    modifiers: Modifiers,
    logo: bool,
}

impl Dispatch<WlPointer, (), State> for PointerState {
    fn event(
        state: &mut State,
        _pointer: &WlPointer,
        event: <WlPointer as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<State>,
    ) {
        use wayland_client::protocol::wl_pointer::Event;
        if matches!(event, Event::Frame) {
            state.pointer_frame();
            return;
        }
        let pending = PointerEvent {
            event,
            modifiers: state.modifiers(),
            logo: state.keyboard.logo(),
        };
        // Coalesce motion without crossing a button or focus transition.
        if matches!(pending.event, Event::Motion { .. })
            && let Some(last) = state.pointer.events.last_mut()
            && matches!(last.event, Event::Motion { .. })
        {
            *last = pending;
        } else {
            state.pointer.events.push(pending);
        }
    }
}

impl State {
    pub(in crate::state) fn release_keyboard_gestures(&mut self) {
        let mut released = 0;
        self.pointer.gesture_bindings.retain(|(input, mask)| {
            if matches!(input, GestureInput::Key(_)) {
                released |= mask;
                false
            } else {
                true
            }
        });
        for (_, mask) in &self.pointer.gesture_bindings {
            released &= !mask;
        }
        if released != 0 {
            self.pointer_sequence(EventSequence {
                released,
                ..Default::default()
            });
        }
    }

    pub(in crate::state) fn keyboard_gesture(
        &mut self,
        key: u32,
        binding: Option<BindingAction>,
        pressed: bool,
        time: u32,
    ) -> bool {
        let mask = binding.and_then(gesture_mask);
        if (pressed && mask.is_none())
            || (!pressed
                && !self
                    .pointer
                    .gesture_bindings
                    .iter()
                    .any(|(input, _)| *input == GestureInput::Key(key)))
        {
            return false;
        }
        self.pointer_frame();
        if pressed && (!self.is_active() || self.pointer.output.is_none()) {
            return true;
        }
        if let Some(mask) = self
            .pointer
            .bind_gesture(GestureInput::Key(key), mask, pressed)
        {
            self.pointer_sequence(EventSequence::button(mask, pressed, time, self.modifiers()));
        }
        if !pressed {
            self.focus_keyboard_on_input();
        }
        pressed
    }

    fn pointer_frame(&mut self) {
        use wayland_client::protocol::wl_pointer::{ButtonState, Event};
        let mut events = std::mem::take(&mut self.pointer.events);
        // Axis metadata describes the whole frame. Apply its complete scroll
        // event at the first axis event, preserving its order relative to clicks.
        let mut scroll = EventSequence::default();
        let mut scroll_at = None;
        for (index, pending) in events.iter().enumerate() {
            if matches!(
                pending.event,
                Event::Axis { .. }
                    | Event::AxisDiscrete { .. }
                    | Event::AxisValue120 { .. }
                    | Event::AxisStop { .. }
            ) {
                if scroll_at.is_none() {
                    scroll_at = Some(index);
                    scroll.modifiers = Some(pending.modifiers);
                }
                scroll.dispatch(&pending.event, Point::default());
            }
        }
        let mut pending_events = events.drain(..).enumerate().peekable();
        while let Some((index, pending)) = pending_events.next() {
            if scroll_at == Some(index) {
                self.flush_pointer_motion();
                self.pointer_sequence(std::mem::take(&mut scroll));
            }
            match pending.event {
                Event::Button {
                    time,
                    button,
                    state: WEnum::Value(button_state),
                    ..
                } => {
                    self.flush_pointer_motion();
                    let Some(button) = MouseButton::from_evdev(button) else {
                        continue;
                    };
                    let pressed = button_state == ButtonState::Pressed;
                    let mask = if pressed {
                        if !self.is_active() || self.pointer.output.is_none() {
                            continue;
                        }
                        let Some(binding) = self.keyboard.mouse_binding(
                            button,
                            self.draw.is_editing_text(),
                            pending.modifiers,
                            pending.logo,
                        ) else {
                            continue;
                        };
                        self.keyboard.reset_compose();
                        let Some(mask) = gesture_mask(binding) else {
                            if self.tablet.input_grab_active() || self.pointer.suppressed_gesture {
                                continue;
                            }
                            match binding {
                                BindingAction::ToggleWhiteboard => self.toggle_whiteboard(),
                                BindingAction::ToggleFreeze => self.toggle_freeze(),
                                _ => {
                                    if let Some(action) = binding.action(pending.modifiers.shift) {
                                        self.apply_action(action);
                                    }
                                }
                            }
                            continue;
                        };
                        Some(mask)
                    } else {
                        None
                    };
                    if let Some(mask) =
                        self.pointer
                            .bind_gesture(GestureInput::Mouse(button), mask, pressed)
                    {
                        self.pointer_sequence(EventSequence::button(
                            mask,
                            pressed,
                            time,
                            pending.modifiers,
                        ));
                    }
                }
                Event::Axis { .. }
                | Event::AxisDiscrete { .. }
                | Event::AxisValue120 { .. }
                | Event::AxisStop { .. } => {}
                event => {
                    let focus = matches!(event, Event::Enter { .. } | Event::Leave { .. });
                    if focus {
                        self.flush_pointer_motion();
                    }
                    if matches!(event, Event::Leave { .. })
                        && pending_events.peek().is_some_and(|(_, next)| {
                            matches!(&next.event, Event::Enter { surface, .. }
                                if self.output_for_surface(surface).is_some())
                        })
                    {
                        continue;
                    }
                    if let Event::Enter { surface, .. } = &event {
                        self.pointer.output = self.output_for_surface(surface);
                        if let Some(output) = self.pointer.output
                            && !self.tablet.input_grab_active()
                        {
                            self.focus_output(output);
                        }
                    }
                    let origin = self
                        .pointer
                        .output
                        .map(|output| self.output_origin(output))
                        .unwrap_or_default();
                    self.pointer.event_sequence.modifiers = Some(pending.modifiers);
                    self.pointer.event_sequence.dispatch(&event, origin);
                    if focus {
                        self.flush_pointer_motion();
                    }
                }
            }
        }
        let sequence = std::mem::take(&mut self.pointer.event_sequence);
        self.pointer_sequence(sequence);
        drop(pending_events);
        self.pointer.events = events;
    }

    fn flush_pointer_motion(&mut self) {
        let sequence = std::mem::take(&mut self.pointer.event_sequence);
        if sequence.motion.is_some() || sequence.enter_serial.is_some() || sequence.left_surface {
            self.pointer_sequence(sequence);
        }
    }

    fn pointer_sequence(&mut self, sequence: EventSequence) {
        let left_surface = sequence.left_surface && sequence.enter_serial.is_none();
        self.pointer.update_state(&sequence);
        // The last moved device owns the cursor and tool preview.
        if sequence.motion.is_some() {
            self.tablet.clear_cursor_tool();
        }
        if sequence.motion.is_some()
            || sequence.pressed & (DRAW_BUTTON | PICKER_BUTTON | ERASER_BUTTON) != 0
        {
            self.pointer.released_pen_tip = None;
        }
        if !self.is_active() || (self.pointer.output.is_none() && sequence.released == 0) {
            if self.is_active() && self.pointer.keyboard_grab_active() {
                return;
            }
            self.interrupt_pointer_gesture();
            if self.pointer.output.is_none() || left_surface {
                self.pointer.clear_focus();
            }
            return;
        }
        // Keep tracking releases, but never let a second device take over a drag.
        if self.tablet.input_grab_active() || self.pointer.suppressed_gesture {
            self.pointer.suppressed_gesture = !self.pointer.gesture_bindings.is_empty();
            if left_surface {
                self.pointer.clear_focus();
            }
            return;
        }
        if let Some(output) = self.pointer.output {
            self.focus_output(output);
        }
        let draw_pressed = sequence.pressed(DRAW_BUTTON);
        let draw_released = sequence.released(DRAW_BUTTON);
        let picker_pressed = sequence.pressed(PICKER_BUTTON);
        let picker_released = sequence.released(PICKER_BUTTON);
        let eraser_pressed = sequence.pressed(ERASER_BUTTON);
        let eraser_released = sequence.released(ERASER_BUTTON);

        let modifiers = sequence.modifiers.unwrap_or_else(|| self.modifiers());
        if self.freeze.capturing() {
            self.pointer.cancel_gesture();
            if left_surface {
                self.pointer.clear_focus();
            }
            return;
        }
        if draw_pressed && let Some(pos) = self.pointer.position {
            self.pointer.draw_press_pos = Some(pos);
            self.pointer.draw_button_in_picker = self.draw.picker_active();
            let clicks = sequence
                .draw_press_time
                .zip(self.pointer.last_draw_click)
                .filter(|(time, (previous, previous_pos, _))| {
                    !self.pointer.draw_button_in_picker
                        && !modifiers.shift
                        && !modifiers.ctrl
                        && time.wrapping_sub(*previous) <= MULTI_CLICK_INTERVAL_MS
                        && pos.distance_squared(*previous_pos) <= CLICK_SLOP_SQUARED
                })
                .map_or(1, |(_, (_, _, count))| count % 3 + 1);
            if clicks == 1 || !self.text_click_at(pos, clicks) {
                self.pointer_down(pos, modifiers, ToolOverride::None);
            }
            self.pointer.last_draw_click = sequence.draw_press_time.map(|time| (time, pos, clicks));
        }
        if picker_pressed && let Some(pos) = self.pointer.position {
            self.pointer.picker_press_time = sequence.picker_button_time;
            self.toggle_picker(pos);
        }
        if eraser_pressed && let Some(pos) = self.pointer.position {
            self.dismiss_picker();
            if let Some(time) = sequence.eraser_button_time {
                self.pointer.eraser_press = Some((time, pos));
            }
        }
        if sequence.motion.is_some()
            && let Some(pos) = self.pointer.position
        {
            if (self.pointer.held(DRAW_BUTTON)
                || (draw_released && self.pointer.draw_press_pos.is_some()))
                && self
                    .pointer
                    .draw_press_pos
                    .is_some_and(|start| pos.distance_squared(start) > CLICK_SLOP_SQUARED)
            {
                self.pointer.last_draw_click = None;
            }
            if (self.pointer.held(ERASER_BUTTON) || eraser_released)
                && !self.pointer.eraser_dragging
                && let Some((_, start)) = self.pointer.eraser_press
                && pos.distance_squared(start) > CLICK_SLOP_SQUARED
            {
                self.pointer.eraser_dragging = true;
                self.pointer_down(start, modifiers, ToolOverride::InvertEraser);
            }
            if (self.draw.picker_active()
                || self.pointer.held(DRAW_BUTTON | PICKER_BUTTON)
                || self.pointer.eraser_dragging)
                && !draw_pressed
                && !picker_pressed
                && !eraser_pressed
            {
                self.pointer_motion(pos, modifiers);
            }
        }
        if let Some(pos) = self.pointer.position {
            if draw_released {
                if !self.pointer.draw_button_in_picker || self.draw.picker_active() {
                    let pen_tip = self.pointer_up(pos, modifiers, false);
                    self.pointer.released_pen_tip = pen_tip;
                }
                self.pointer.draw_button_in_picker = false;
                self.pointer.draw_press_pos = None;
            }
            if picker_released {
                let latch_picker = short_click(
                    self.pointer.picker_press_time.take(),
                    sequence.picker_button_time,
                );
                self.pointer_up(pos, modifiers, latch_picker);
            }
            if eraser_released {
                let dragging = std::mem::take(&mut self.pointer.eraser_dragging);
                let clicked =
                    self.pointer.eraser_press.take().is_some_and(|(time, _)| {
                        short_click(Some(time), sequence.eraser_button_time)
                    });
                if dragging {
                    self.pointer_up(pos, modifiers, false);
                } else if clicked {
                    self.apply_action(Action::ToggleEraser);
                }
            }
            if self.draw.picker_active() {
                if sequence.vertical_axis_stopped {
                    self.pointer.reset_scroll();
                }
            } else {
                let steps = self.pointer.scroll_steps(&sequence, modifiers);
                if steps != 0.0 && self.adjust(steps, pos, modifiers) {
                    self.pointer.stop_scroll(steps, modifiers);
                }
            }
        }
        if left_surface {
            if self.pointer.keyboard_grab_active() {
                self.pointer.output = None;
                self.pointer.cursor_serial = None;
                self.pointer.current_cursor = None;
            } else {
                self.interrupt_pointer_gesture();
                self.pointer.clear_focus();
            }
        }
        self.refresh_cursor();
        if (sequence.pressed | sequence.released) & (DRAW_BUTTON | PICKER_BUTTON | ERASER_BUTTON)
            != 0
        {
            self.update_output_input();
        }
    }
}

pub(super) fn update_cursor(
    serial: Option<u32>,
    device: Option<&WpCursorShapeDeviceV1>,
    current: &mut Option<Cursor>,
    cursor: Cursor,
    hide: impl FnOnce(u32),
) {
    let (Some(serial), Some(device)) = (serial, device) else {
        return;
    };
    if current.is_some_and(|current| current.same_compositor_cursor(cursor)) {
        return;
    }
    match cursor.shape() {
        Some(hint) => device.set_shape(serial, cursor_shape(hint)),
        None => hide(serial),
    }
    *current = Some(cursor);
}

fn gesture_mask(binding: BindingAction) -> Option<u8> {
    binding.pointer_gesture().then(|| match binding {
        BindingAction::Draw => DRAW_BUTTON,
        BindingAction::OpenPicker => PICKER_BUTTON,
        BindingAction::Erase => ERASER_BUTTON,
        _ => unreachable!("unmapped pointer gesture"),
    })
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

#[derive(Default)]
struct EventSequence {
    modifiers: Option<Modifiers>,
    motion: Option<Point>,

    pressed: u8,
    released: u8,
    draw_press_time: Option<u32>,
    picker_button_time: Option<u32>,
    eraser_button_time: Option<u32>,
    axis_vertical: f64,
    axis_discrete: i32,
    axis_value120: i32,
    axis_time: Option<u32>,
    vertical_axis_stopped: bool,

    enter_serial: Option<u32>,
    left_surface: bool,
}

impl EventSequence {
    fn pressed(&self, button: u8) -> bool {
        self.pressed & button != 0
    }

    fn released(&self, button: u8) -> bool {
        self.released & button != 0
    }

    fn scroll_steps(&self) -> f64 {
        if self.axis_value120 != 0 {
            self.axis_value120 as f64 / 120.0
        } else if self.axis_discrete != 0 {
            self.axis_discrete as f64
        } else {
            self.axis_vertical / 10.0
        }
    }

    fn is_wheel(&self) -> bool {
        self.axis_value120 != 0 || self.axis_discrete != 0
    }

    fn button(mask: u8, pressed: bool, time: u32, modifiers: Modifiers) -> Self {
        Self {
            modifiers: Some(modifiers),
            pressed: if pressed { mask } else { 0 },
            released: if pressed { 0 } else { mask },
            draw_press_time: (pressed && mask == DRAW_BUTTON).then_some(time),
            picker_button_time: (mask == PICKER_BUTTON).then_some(time),
            eraser_button_time: (mask == ERASER_BUTTON).then_some(time),
            ..Self::default()
        }
    }

    fn dispatch(&mut self, event: &<WlPointer as Proxy>::Event, origin: Point) {
        use wayland_client::protocol::wl_pointer::Event;
        match event {
            Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                self.enter_serial = Some(*serial);
                self.motion = Some(Point::new(*surface_x as f32, *surface_y as f32) + origin);
            }
            Event::Leave { .. } => self.left_surface = true,
            Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                self.motion = Some(Point::new(*surface_x as f32, *surface_y as f32) + origin);
            }
            Event::Axis {
                axis: WEnum::Value(wayland_client::protocol::wl_pointer::Axis::VerticalScroll),
                time,
                value,
            } => {
                self.axis_vertical += value;
                self.axis_time = Some(*time);
            }
            Event::AxisDiscrete {
                axis: WEnum::Value(wayland_client::protocol::wl_pointer::Axis::VerticalScroll),
                discrete,
            } => self.axis_discrete += discrete,
            Event::AxisValue120 {
                axis: WEnum::Value(wayland_client::protocol::wl_pointer::Axis::VerticalScroll),
                value120,
            } => self.axis_value120 += value120,
            Event::AxisStop {
                axis: WEnum::Value(wayland_client::protocol::wl_pointer::Axis::VerticalScroll),
                ..
            } => self.vertical_axis_stopped = true,
            _ => {}
        }
    }
}

wayland_client::delegate_dispatch!(State: [WlPointer: ()] => PointerState);
wayland_client::delegate_noop!(State: ignore WpCursorShapeDeviceV1);
