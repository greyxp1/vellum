use std::time::{Duration, Instant};
use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::Proxy;
use wayland_client::QueueHandle;
use wayland_client::WEnum;

use wayland_client::protocol::wl_pointer::WlPointer;

use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;

use super::short_click;
use crate::OutputId;
use crate::draw::{Action, Cursor, CursorHint, Modifiers, ToolOverride};
use crate::state::State;

const EVDEV_LEFT: u32 = 272;
const EVDEV_RIGHT: u32 = 273;
const EVDEV_MIDDLE: u32 = 274;
const EVDEV_SIDE: u32 = 275;
const EVDEV_EXTRA: u32 = 276;
const EVDEV_FORWARD: u32 = 277;
const EVDEV_BACK: u32 = 278;
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const MIDDLE: u8 = 4;
const UNDO: u8 = 8;
const REDO: u8 = 16;
const CLICK_SLOP_SQUARED: f64 = 36.0;
const SCROLL_BURST_TIMEOUT_MS: u32 = 100;
const WHEEL_ACCEL_SAMPLE_COUNT: usize = 3;
const WHEEL_ACCEL_MAX_GAIN: f64 = 8.0;
const WHEEL_ACCEL_CURVE_GAIN: f64 = 11.5;
const WHEEL_ACCEL_CURVE_SCALE: f64 = 12.0;

#[derive(Default)]
pub(in crate::state) struct PointerState {
    event_sequence: EventSequence,

    cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    cursor_serial: Option<u32>,
    current_cursor: Option<Cursor>,
    output: Option<OutputId>,
    position: Option<(f64, f64)>,
    left_button_held: bool,
    right_button_held: bool,
    middle_button_held: bool,
    suppressed_gesture: bool,
    left_button_in_picker: bool,
    left_press_pos: Option<(f64, f64)>,
    right_press_time: Option<u32>,
    middle_press: Option<(u32, (f64, f64))>,
    middle_dragging: bool,
    last_left_click: Option<(u32, (f64, f64), u8)>,
    scroll_remainder: f64,
    wheel_acceleration: WheelAcceleration,
    scroll_stop: Option<ScrollStop>,
}

impl PointerState {
    pub(in crate::state) fn set_cursor_device(
        &mut self,
        cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    ) {
        self.cursor_shape_device = cursor_shape_device;
    }

    pub(in crate::state) fn position(&self) -> Option<(f64, f64)> {
        self.position
    }

    pub(in crate::state) fn tool_override(&self) -> ToolOverride {
        if self.middle_button_held {
            ToolOverride::InvertEraser
        } else {
            ToolOverride::None
        }
    }

    pub(in crate::state) fn tool_cursor_supported(&self) -> bool {
        self.cursor_shape_device.is_some()
    }

    pub(in crate::state) fn input_grab_active(&self) -> bool {
        !self.suppressed_gesture
            && (self.left_button_held || self.right_button_held || self.middle_button_held)
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

    pub(in crate::state) fn remove_output(&mut self, output: OutputId) -> Option<(f64, f64)> {
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
        self.cursor_serial = None;
    }

    pub(in crate::state) fn cancel_gesture(&mut self) {
        self.event_sequence = EventSequence {
            motion: self.event_sequence.motion,
            enter_serial: self.event_sequence.enter_serial,
            left_surface: self.event_sequence.left_surface,
            ..Default::default()
        };
        self.left_button_held = false;
        self.right_button_held = false;
        self.middle_button_held = false;
        self.suppressed_gesture = false;
        self.left_button_in_picker = false;
        self.left_press_pos = None;
        self.right_press_time = None;
        self.middle_press = None;
        self.middle_dragging = false;
        self.last_left_click = None;
        self.reset_scroll();
    }

    fn update_state(&mut self, sequence: EventSequence) {
        if let Some(new_pos) = sequence.motion {
            self.position = Some(new_pos);
        }

        if let Some(serial) = sequence.enter_serial {
            self.cursor_serial = Some(serial);
            self.current_cursor = None;
        }

        update_button(&mut self.left_button_held, sequence, LEFT);
        update_button(&mut self.right_button_held, sequence, RIGHT);
        update_button(&mut self.middle_button_held, sequence, MIDDLE);
    }

    fn scroll_steps(&mut self, sequence: EventSequence, modifiers: Modifiers) -> f32 {
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
        if !state.active
            && !matches!(
                event,
                Event::Enter { .. } | Event::Leave { .. } | Event::Motion { .. } | Event::Frame
            )
        {
            return;
        }
        if let Event::Enter { surface, .. } = &event {
            state.pointer.output = state.output_for_surface(surface);
            if let Some(output) = state.pointer.output
                && !state.tablet.input_grab_active()
            {
                state.focus_output(output);
            }
        }
        let origin = state
            .pointer
            .output
            .map(|output| state.output_origin(output))
            .unwrap_or_default();
        if let Some(sequence) = state
            .pointer
            .event_sequence
            .dispatch(event, (f64::from(origin.x), f64::from(origin.y)))
        {
            state.pointer.update_state(sequence);
            if !state.active || state.pointer.output.is_none() {
                state.pointer.cancel_gesture();
                if state.pointer.output.is_none()
                    || (sequence.left_surface && sequence.enter_serial.is_none())
                {
                    state.pointer.clear_focus();
                }
                return;
            }
            // Keep tracking releases, but never let a second device take over a drag.
            if state.tablet.input_grab_active() || state.pointer.suppressed_gesture {
                state.pointer.suppressed_gesture = state.pointer.left_button_held
                    || state.pointer.right_button_held
                    || state.pointer.middle_button_held;
                if sequence.left_surface && sequence.enter_serial.is_none() {
                    state.pointer.clear_focus();
                }
                return;
            }
            if let Some(output) = state.pointer.output {
                state.focus_output(output);
            }
            let left_pressed = sequence.pressed(LEFT);
            let left_released = sequence.released(LEFT);
            let right_pressed = sequence.pressed(RIGHT);
            let right_released = sequence.released(RIGHT);
            let middle_pressed = sequence.pressed(MIDDLE);
            let middle_released = sequence.released(MIDDLE);

            if sequence.pressed(UNDO) {
                state.apply_action(Action::Undo);
            }
            if sequence.pressed(REDO) {
                state.apply_action(Action::Redo);
            }

            let modifiers = state.modifiers();
            if left_pressed && let Some(pos) = state.pointer.position {
                state.pointer.left_press_pos = Some(pos);
                state.pointer.left_button_in_picker = state.draw.picker_active();
                let clicks = sequence
                    .left_press_time
                    .zip(state.pointer.last_left_click)
                    .filter(|(time, (previous, previous_pos, _))| {
                        !state.pointer.left_button_in_picker
                            && !modifiers.shift
                            && !modifiers.ctrl
                            && time.wrapping_sub(*previous) <= 400
                            && distance_squared(pos, *previous_pos) <= CLICK_SLOP_SQUARED
                    })
                    .map_or(1, |(_, (_, _, count))| count % 3 + 1);
                if clicks == 1 || !state.text_click_at(pos, clicks) {
                    state.pointer_down(pos, modifiers, ToolOverride::None);
                }
                state.pointer.last_left_click =
                    sequence.left_press_time.map(|time| (time, pos, clicks));
            }
            if right_pressed && let Some(pos) = state.pointer.position {
                state.pointer.right_press_time = sequence.right_button_time;
                state.toggle_picker(pos);
            }
            if middle_pressed && let Some(pos) = state.pointer.position {
                state.dismiss_picker();
                if let Some(time) = sequence.middle_button_time {
                    state.pointer.middle_press = Some((time, pos));
                }
            }
            if sequence.motion.is_some()
                && let Some(pos) = state.pointer.position
            {
                if (state.pointer.left_button_held
                    || (left_released && state.pointer.left_press_pos.is_some()))
                    && state
                        .pointer
                        .left_press_pos
                        .is_some_and(|start| distance_squared(pos, start) > CLICK_SLOP_SQUARED)
                {
                    state.pointer.last_left_click = None;
                }
                if (state.pointer.middle_button_held || middle_released)
                    && !state.pointer.middle_dragging
                    && let Some((_, start)) = state.pointer.middle_press
                    && distance_squared(pos, start) > CLICK_SLOP_SQUARED
                {
                    state.pointer.middle_dragging = true;
                    state.pointer_down(start, modifiers, ToolOverride::InvertEraser);
                }
                if (state.draw.picker_active()
                    || state.pointer.left_button_held
                    || state.pointer.right_button_held
                    || state.pointer.middle_dragging)
                    && !left_pressed
                    && !right_pressed
                    && !middle_pressed
                {
                    state.pointer_motion(pos, modifiers);
                }
            }
            if let Some(pos) = state.pointer.position {
                if left_released {
                    if !state.pointer.left_button_in_picker || state.draw.picker_active() {
                        state.pointer_up(pos, modifiers, false);
                    }
                    state.pointer.left_button_in_picker = false;
                    state.pointer.left_press_pos = None;
                }
                if right_released {
                    let latch_picker = short_click(
                        state.pointer.right_press_time.take(),
                        sequence.right_button_time,
                    );
                    state.pointer_up(pos, modifiers, latch_picker);
                }
                if middle_released {
                    let dragging = std::mem::take(&mut state.pointer.middle_dragging);
                    let clicked = state.pointer.middle_press.take().is_some_and(|(time, _)| {
                        short_click(Some(time), sequence.middle_button_time)
                    });
                    if dragging {
                        state.pointer_up(pos, modifiers, false);
                    } else if clicked {
                        state.apply_action(Action::ToggleEraser);
                    }
                }
                if state.draw.picker_active() {
                    if sequence.vertical_axis_stopped {
                        state.pointer.reset_scroll();
                    }
                } else {
                    let steps = state.pointer.scroll_steps(sequence, modifiers);
                    if steps != 0.0 && state.adjust(steps, pos, modifiers) {
                        state.pointer.stop_scroll(steps, modifiers);
                    }
                }
            }
            if sequence.left_surface && sequence.enter_serial.is_none() {
                state.pointer.clear_focus();
            }
            state.refresh_cursor();
            if (sequence.pressed | sequence.released) & (LEFT | RIGHT | MIDDLE) != 0 {
                state.update_output_input();
            }
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
    match cursor {
        Cursor::Shape(hint) => device.set_shape(serial, cursor_shape(hint)),
        Cursor::Hidden | Cursor::Tool(_) => hide(serial),
    }
    *current = Some(cursor);
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

#[derive(Default, Clone, Copy)]
struct EventSequence {
    motion: Option<(f64, f64)>,

    pressed: u8,
    released: u8,
    left_press_time: Option<u32>,
    right_button_time: Option<u32>,
    middle_button_time: Option<u32>,
    axis_vertical: f64,
    axis_discrete: i32,
    axis_value120: i32,
    axis_time: Option<u32>,
    vertical_axis_stopped: bool,

    enter_serial: Option<u32>,
    left_surface: bool,
}

impl EventSequence {
    fn pressed(self, button: u8) -> bool {
        self.pressed & button != 0
    }

    fn released(self, button: u8) -> bool {
        self.released & button != 0
    }

    fn scroll_steps(self) -> f64 {
        if self.axis_value120 != 0 {
            self.axis_value120 as f64 / 120.0
        } else if self.axis_discrete != 0 {
            self.axis_discrete as f64
        } else {
            self.axis_vertical / 10.0
        }
    }

    fn is_wheel(self) -> bool {
        self.axis_value120 != 0 || self.axis_discrete != 0
    }

    fn dispatch(&mut self, event: <WlPointer as Proxy>::Event, origin: (f64, f64)) -> Option<Self> {
        use wayland_client::protocol::wl_pointer::Event;
        match event {
            Event::Enter {
                serial,
                surface: _,
                surface_x,
                surface_y,
            } => {
                self.enter_serial = Some(serial);
                self.motion = Some((surface_x + origin.0, surface_y + origin.1));
            }
            Event::Leave { .. } => {
                self.left_surface = true;
            }
            Event::Motion {
                time: _,
                surface_x,
                surface_y,
            } => {
                self.motion = Some((surface_x + origin.0, surface_y + origin.1));
            }
            Event::Button {
                serial: _,
                time,
                button,
                state: button_state,
            } => {
                use wayland_client::protocol::wl_pointer::ButtonState;
                let transition = match button_state {
                    WEnum::Value(ButtonState::Pressed) => &mut self.pressed,
                    WEnum::Value(ButtonState::Released) => &mut self.released,
                    _ => return None,
                };
                let mask = match button {
                    EVDEV_LEFT => LEFT,
                    EVDEV_RIGHT => RIGHT,
                    EVDEV_MIDDLE => MIDDLE,
                    EVDEV_SIDE | EVDEV_BACK => UNDO,
                    EVDEV_EXTRA | EVDEV_FORWARD => REDO,
                    _ => return None,
                };
                *transition |= mask;
                if mask == LEFT && matches!(button_state, WEnum::Value(ButtonState::Pressed)) {
                    self.left_press_time = Some(time);
                }
                if mask == RIGHT {
                    self.right_button_time = Some(time);
                }
                if mask == MIDDLE {
                    self.middle_button_time = Some(time);
                }
            }
            Event::Axis {
                axis: WEnum::Value(wayland_client::protocol::wl_pointer::Axis::VerticalScroll),
                time,
                value,
            } => {
                self.axis_vertical += value;
                self.axis_time = Some(time);
            }
            Event::AxisDiscrete {
                axis: WEnum::Value(wayland_client::protocol::wl_pointer::Axis::VerticalScroll),
                discrete,
            } => {
                self.axis_discrete += discrete;
            }
            Event::AxisValue120 {
                axis: WEnum::Value(wayland_client::protocol::wl_pointer::Axis::VerticalScroll),
                value120,
            } => {
                self.axis_value120 += value120;
            }
            Event::AxisStop {
                axis: WEnum::Value(wayland_client::protocol::wl_pointer::Axis::VerticalScroll),
                ..
            } => {
                self.vertical_axis_stopped = true;
            }
            Event::Frame => return Some(std::mem::take(self)),
            _ => {}
        }
        None
    }
}

fn update_button(held: &mut bool, sequence: EventSequence, button: u8) {
    if sequence.pressed(button) {
        *held = true;
    }
    if sequence.released(button) {
        *held = false;
    }
}

fn distance_squared(first: (f64, f64), second: (f64, f64)) -> f64 {
    (first.0 - second.0).powi(2) + (first.1 - second.1).powi(2)
}

wayland_client::delegate_dispatch!(State: [WlPointer: ()] => PointerState);
wayland_client::delegate_noop!(State: ignore WpCursorShapeDeviceV1);
