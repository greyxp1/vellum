use std::time::{Duration, Instant};

use wayland_client::protocol::wl_pointer::{Axis, ButtonState, Event, WlPointer};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;

use super::{CursorFocus, MULTI_CLICK_INTERVAL_MS, short_click};
use crate::OutputId;
use crate::config::{BindingAction, MouseButton};
use crate::draw::{Action, DrawState, Modifiers, Point, ToolOverride};
use crate::state::State;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Gesture {
    Draw,
    Picker,
    Eraser,
}

impl Gesture {
    fn from_binding(binding: BindingAction) -> Option<Self> {
        match binding {
            BindingAction::Draw => Some(Self::Draw),
            BindingAction::OpenPicker => Some(Self::Picker),
            BindingAction::Erase => Some(Self::Eraser),
            _ => None,
        }
    }
}

/// One logical pointer change, applied in frame order.
#[derive(Clone, Copy)]
enum PointerInput {
    Motion(Point),
    Leave,
    Press(Gesture, u32),
    Release(Gesture, Option<u32>),
    Scroll(Scroll),
}

#[derive(Default)]
pub(in crate::state) struct PointerState {
    events: Vec<PointerEvent>,
    bindings: Vec<(GestureInput, Gesture)>,
    pub(super) focus: CursorFocus,
    pub(super) released_pen_tip: Option<Point>,
    suppressed_gesture: bool,
    draw_in_picker: bool,
    draw_press: Option<Point>,
    picker_press: Option<u32>,
    eraser_press: Option<(u32, Point)>,
    eraser_dragging: bool,
    last_draw_click: Option<(u32, Point, u8)>,
    scroll_remainder: f64,
    wheel_acceleration: WheelAcceleration,
    scroll_stop: Option<ScrollStop>,
}

impl PointerState {
    fn held(&self, gesture: Gesture) -> bool {
        self.bindings.iter().any(|&(_, held)| held == gesture)
    }

    /// Records a binding change, returning a gesture's first press or last release.
    fn bind_gesture(
        &mut self,
        input: GestureInput,
        gesture: Option<Gesture>,
        pressed: bool,
        time: u32,
    ) -> Option<PointerInput> {
        let index = self.bindings.iter().position(|&(held, _)| held == input);
        let gesture = match (index, pressed) {
            (None, true) => {
                let gesture = gesture?;
                self.bindings.push((input, gesture));
                gesture
            }
            (Some(index), false) => self.bindings.remove(index).1,
            _ => return None,
        };
        let held = self.bindings.iter().filter(|&&(_, held)| held == gesture);
        (held.count() == usize::from(pressed)).then_some(if pressed {
            PointerInput::Press(gesture, time)
        } else {
            PointerInput::Release(gesture, Some(time))
        })
    }

    pub(in crate::state) fn input_grab_active(&self) -> bool {
        !self.suppressed_gesture && !self.bindings.is_empty()
    }

    pub(in crate::state) fn keyboard_grab_active(&self) -> bool {
        !self.suppressed_gesture
            && self
                .bindings
                .iter()
                .any(|(input, _)| matches!(input, GestureInput::Key(_)))
    }

    pub(super) fn show_cursor(
        &mut self,
        draw: &mut DrawState,
        pointer: &WlPointer,
    ) -> Option<bool> {
        let tool_override = if self.held(Gesture::Eraser) {
            ToolOverride::InvertEraser
        } else {
            ToolOverride::None
        };
        self.focus
            .show(draw, tool_override, self.released_pen_tip, |serial| {
                pointer.set_cursor(serial, None, 0, 0)
            })
    }

    pub(in crate::state) fn restore_cursor(&mut self) {
        self.focus.restore();
    }

    pub(super) fn clear_pointer(&mut self) {
        if let Some(device) = self.focus.device.take() {
            device.destroy();
        }
        *self = Self::default();
    }

    pub(in crate::state) fn remove_output(&mut self, output: OutputId) -> Option<Point> {
        if self.focus.output != Some(output) {
            return None;
        }
        let end_position = self.input_grab_active().then_some(self.focus.position);
        self.cancel_gesture(false);
        self.clear_focus();
        end_position.flatten()
    }

    fn clear_focus(&mut self) {
        self.focus.clear();
        self.released_pen_tip = None;
    }

    /// Ends the gesture. Suppression keeps held bindings so their releases drain.
    pub(super) fn cancel_gesture(&mut self, suppress: bool) {
        if !suppress {
            self.bindings.clear();
        }
        *self = Self {
            suppressed_gesture: !self.bindings.is_empty(),
            events: std::mem::take(&mut self.events),
            bindings: std::mem::take(&mut self.bindings),
            focus: std::mem::take(&mut self.focus),
            ..Self::default()
        };
    }

    fn scroll_steps(&mut self, scroll: Scroll, modifiers: Modifiers) -> f32 {
        let input = scroll.steps();
        let mut steps = 0.0;
        if input == 0.0 || !self.scroll_stop_blocks(-input, modifiers) {
            if input != 0.0 && !scroll.is_wheel() {
                self.wheel_acceleration.reset();
            }
            self.scroll_remainder -= input;
            steps = self.scroll_remainder.trunc();
            self.scroll_remainder -= steps;
            if steps != 0.0 && scroll.is_wheel() {
                steps = self.wheel_acceleration.apply(scroll.time, steps, modifiers);
            }
        }
        if scroll.stopped {
            self.reset_scroll();
        }
        steps as f32
    }

    fn scroll_stop_blocks(&mut self, direction: f64, modifiers: Modifiers) -> bool {
        let Some(stop) = &mut self.scroll_stop else {
            return false;
        };
        let within_burst =
            stop.last_event.elapsed() <= Duration::from_millis(u64::from(SCROLL_BURST_TIMEOUT_MS));
        if stop.positive == direction.is_sign_positive()
            && stop.modifiers == (modifiers.ctrl, modifiers.shift)
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

/// Vertical scroll accumulated over one frame.
#[derive(Clone, Copy, Default)]
struct Scroll {
    vertical: f64,
    discrete: i32,
    value120: i32,
    time: Option<u32>,
    stopped: bool,
}

impl Scroll {
    /// Adds an axis event, returning false for other events.
    fn add(&mut self, event: &Event) -> bool {
        const VERTICAL: WEnum<Axis> = WEnum::Value(Axis::VerticalScroll);
        match *event {
            Event::Axis { axis, time, value } if axis == VERTICAL => {
                self.vertical += value;
                self.time = Some(time);
            }
            Event::AxisDiscrete { axis, discrete } if axis == VERTICAL => self.discrete += discrete,
            Event::AxisValue120 { axis, value120 } if axis == VERTICAL => self.value120 += value120,
            Event::AxisStop { axis, .. } if axis == VERTICAL => self.stopped = true,
            Event::Axis { .. }
            | Event::AxisDiscrete { .. }
            | Event::AxisValue120 { .. }
            | Event::AxisStop { .. } => {}
            _ => return false,
        }
        true
    }

    fn steps(self) -> f64 {
        if self.value120 != 0 {
            self.value120 as f64 / 120.0
        } else if self.discrete != 0 {
            self.discrete as f64
        } else {
            self.vertical / 10.0
        }
    }

    fn is_wheel(self) -> bool {
        self.value120 != 0 || self.discrete != 0
    }
}

struct PointerEvent {
    event: Event,
    modifiers: Modifiers,
    logo: bool,
}

impl Dispatch<WlPointer, (), State> for PointerState {
    fn event(
        state: &mut State,
        _pointer: &WlPointer,
        event: Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<State>,
    ) {
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
        let was_held = [Gesture::Draw, Gesture::Picker, Gesture::Eraser]
            .map(|gesture| (gesture, self.pointer.held(gesture)));
        self.pointer
            .bindings
            .retain(|(input, _)| matches!(input, GestureInput::Mouse(_)));
        for (gesture, was_held) in was_held {
            if was_held && !self.pointer.held(gesture) {
                self.pointer_input(PointerInput::Release(gesture, None), self.modifiers());
            }
        }
    }

    pub(in crate::state) fn keyboard_gesture(
        &mut self,
        key: u32,
        binding: Option<BindingAction>,
        pressed: bool,
        time: u32,
    ) -> bool {
        let input = GestureInput::Key(key);
        let gesture = binding.and_then(Gesture::from_binding);
        if (pressed && gesture.is_none())
            || (!pressed && !self.pointer.bindings.iter().any(|(held, _)| *held == input))
        {
            return false;
        }
        self.pointer_frame();
        if pressed && (!self.is_active() || self.pointer.focus.output.is_none()) {
            return true;
        }
        if let Some(input) = self.pointer.bind_gesture(input, gesture, pressed, time) {
            self.pointer_input(input, self.modifiers());
        }
        if !pressed {
            self.focus_keyboard_on_input();
        }
        pressed
    }

    fn pointer_frame(&mut self) {
        let mut events = std::mem::take(&mut self.pointer.events);
        // Axis metadata describes the whole frame. Apply its complete scroll
        // at the first axis event, preserving its order relative to clicks.
        let mut scroll = Scroll::default();
        let mut scroll_at = None;
        for (index, pending) in events.iter().enumerate() {
            if scroll.add(&pending.event) {
                scroll_at.get_or_insert((index, pending.modifiers));
            }
        }
        let mut motion = None;
        let mut pending_events = events.drain(..).enumerate().peekable();
        while let Some((index, pending)) = pending_events.next() {
            let modifiers = pending.modifiers;
            let scroll_here = scroll_at.filter(|&(at, _)| at == index);
            if (scroll_here.is_some()
                || matches!(
                    pending.event,
                    Event::Button { .. } | Event::Enter { .. } | Event::Leave { .. }
                ))
                && let Some((point, modifiers)) = motion.take()
            {
                self.pointer_input(PointerInput::Motion(point), modifiers);
            }
            if let Some((_, modifiers)) = scroll_here {
                self.pointer_input(PointerInput::Scroll(scroll), modifiers);
            }
            match pending.event {
                Event::Button {
                    serial,
                    time,
                    button,
                    state: WEnum::Value(button_state),
                    ..
                } => {
                    let Some(button) = MouseButton::from_evdev(button) else {
                        continue;
                    };
                    let pressed = button_state == ButtonState::Pressed;
                    let gesture = if !pressed {
                        None
                    } else if !self.is_active() || self.pointer.focus.output.is_none() {
                        continue;
                    } else {
                        self.clipboard.serial = Some(serial);
                        let editing = self.draw.is_editing_text();
                        let Some(binding) =
                            self.keyboard
                                .mouse_binding(button, editing, modifiers, pending.logo)
                        else {
                            continue;
                        };
                        self.keyboard.reset_compose();
                        let gesture = Gesture::from_binding(binding);
                        if gesture.is_none()
                            && !self.tablet.input_grab_active()
                            && !self.pointer.suppressed_gesture
                            && !self.toggle_binding(binding)
                            && let Some(action) = binding.action(modifiers.shift)
                        {
                            self.apply_action(action);
                        }
                        gesture
                    };
                    let input = GestureInput::Mouse(button);
                    if let Some(input) = self.pointer.bind_gesture(input, gesture, pressed, time) {
                        self.pointer_input(input, modifiers);
                    }
                }
                Event::Motion {
                    surface_x,
                    surface_y,
                    ..
                } => motion = Some((self.pointer_point(surface_x, surface_y), modifiers)),
                // Crossing between overlay outputs keeps the gesture alive.
                Event::Leave { .. }
                    if pending_events.peek().is_some_and(|(_, next)| {
                        matches!(&next.event, Event::Enter { surface, .. }
                            if self.output_for_surface(surface).is_some())
                    }) => {}
                Event::Leave { .. } => self.pointer_input(PointerInput::Leave, modifiers),
                Event::Enter {
                    serial,
                    surface,
                    surface_x,
                    surface_y,
                } => {
                    let output = self.output_for_surface(&surface);
                    self.pointer.focus.enter(output, serial);
                    if let Some(output) = output
                        && !self.tablet.input_grab_active()
                    {
                        self.focus_output(output);
                    }
                    let point = self.pointer_point(surface_x, surface_y);
                    self.pointer_input(PointerInput::Motion(point), modifiers);
                }
                _ => {}
            }
        }
        if let Some((point, modifiers)) = motion {
            self.pointer_input(PointerInput::Motion(point), modifiers);
        }
        drop(pending_events);
        self.pointer.events = events;
    }

    fn pointer_point(&self, x: f64, y: f64) -> Point {
        let origin = self
            .pointer
            .focus
            .output
            .map(|output| self.output_origin(output));
        Point::new(x as f32, y as f32) + origin.unwrap_or_default()
    }

    fn pointer_input(&mut self, input: PointerInput, modifiers: Modifiers) {
        let left = matches!(input, PointerInput::Leave);
        match input {
            PointerInput::Motion(point) => {
                self.pointer.focus.position = Some(point);
                // The last moved device owns the cursor and tool preview.
                self.tablet.clear_cursor_tool();
                self.pointer.released_pen_tip = None;
            }
            PointerInput::Press(..) => self.pointer.released_pen_tip = None,
            _ => {}
        }
        let releasing = matches!(input, PointerInput::Release(..));
        if !self.is_active() || (self.pointer.focus.output.is_none() && !releasing) {
            if self.is_active() && self.pointer.keyboard_grab_active() {
                return;
            }
            self.interrupt_pointer_gesture();
            if self.pointer.focus.output.is_none() || left {
                self.pointer.clear_focus();
            }
            return;
        }
        // Keep tracking releases, but never let a second device take over a drag.
        let suppressed = self.tablet.input_grab_active() || self.pointer.suppressed_gesture;
        if suppressed {
            self.pointer.suppressed_gesture = !self.pointer.bindings.is_empty();
        } else if let Some(output) = self.pointer.focus.output {
            self.focus_output(output);
        }
        // Selecting the first output can start an activation-time capture.
        if suppressed || self.freeze.capturing() {
            if !suppressed {
                self.pointer.cancel_gesture(false);
            }
            if left {
                self.pointer.clear_focus();
            }
            return;
        }
        if let Some(pos) = self.pointer.focus.position {
            self.pointer_gesture(input, pos, modifiers);
        }
        if left && self.pointer.keyboard_grab_active() {
            self.pointer.focus.leave();
        } else if left {
            self.interrupt_pointer_gesture();
            self.pointer.clear_focus();
        }
        self.refresh_cursor();
        if matches!(input, PointerInput::Press(..) | PointerInput::Release(..)) {
            self.update_output_input();
        }
    }

    fn pointer_gesture(&mut self, input: PointerInput, pos: Point, modifiers: Modifiers) {
        let moved = |start: Point| pos.distance_squared(start) > CLICK_SLOP_SQUARED;
        let pointer = &mut self.pointer;
        match input {
            PointerInput::Motion(_) => {
                if pointer.held(Gesture::Draw) && pointer.draw_press.is_some_and(moved) {
                    pointer.last_draw_click = None;
                }
                if pointer.held(Gesture::Eraser)
                    && !pointer.eraser_dragging
                    && let Some((_, start)) = pointer.eraser_press
                    && moved(start)
                {
                    pointer.eraser_dragging = true;
                    self.pointer_down(start, modifiers, ToolOverride::InvertEraser);
                }
                if self.draw.picker_active()
                    || self.pointer.held(Gesture::Draw)
                    || self.pointer.held(Gesture::Picker)
                    || self.pointer.eraser_dragging
                {
                    self.pointer_motion(pos, modifiers);
                }
            }
            PointerInput::Press(Gesture::Draw, time) => {
                pointer.draw_press = Some(pos);
                pointer.draw_in_picker = self.draw.picker_active();
                let clicks = pointer
                    .last_draw_click
                    .filter(|(previous, previous_pos, _)| {
                        !pointer.draw_in_picker
                            && !modifiers.shift
                            && !modifiers.ctrl
                            && time.wrapping_sub(*previous) <= MULTI_CLICK_INTERVAL_MS
                            && pos.distance_squared(*previous_pos) <= CLICK_SLOP_SQUARED
                    })
                    .map_or(1, |(_, _, count)| count % 3 + 1);
                pointer.last_draw_click = Some((time, pos, clicks));
                if clicks == 1 || !self.text_click_at(pos, clicks) {
                    self.pointer_down(pos, modifiers, ToolOverride::None);
                }
            }
            PointerInput::Press(Gesture::Picker, time) => {
                pointer.picker_press = Some(time);
                self.toggle_picker(pos);
            }
            PointerInput::Press(Gesture::Eraser, time) => {
                pointer.eraser_press = Some((time, pos));
                self.dismiss_picker();
            }
            PointerInput::Release(Gesture::Draw, _) => {
                pointer.draw_press = None;
                if !std::mem::take(&mut pointer.draw_in_picker) || self.draw.picker_active() {
                    self.pointer.released_pen_tip = self.pointer_up(pos, modifiers, false);
                }
            }
            PointerInput::Release(Gesture::Picker, time) => {
                let latch_picker = short_click(pointer.picker_press.take(), time);
                self.pointer_up(pos, modifiers, latch_picker);
            }
            PointerInput::Release(Gesture::Eraser, time) => {
                let press = pointer.eraser_press.take().map(|(time, _)| time);
                if std::mem::take(&mut pointer.eraser_dragging) {
                    self.pointer_up(pos, modifiers, false);
                } else if short_click(press, time) {
                    self.apply_action(Action::ToggleEraser);
                }
            }
            PointerInput::Scroll(scroll) if self.draw.picker_active() => {
                if scroll.stopped {
                    pointer.reset_scroll();
                }
            }
            PointerInput::Scroll(scroll) => {
                let steps = pointer.scroll_steps(scroll, modifiers);
                if steps != 0.0 && self.adjust(steps, pos, modifiers) {
                    self.pointer.stop_scroll(steps, modifiers);
                }
            }
            PointerInput::Leave => {}
        }
    }
}

wayland_client::delegate_dispatch!(State: [WlPointer: ()] => PointerState);
wayland_client::delegate_noop!(State: ignore WpCursorShapeDeviceV1);
