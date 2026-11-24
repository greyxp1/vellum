use std::collections::HashMap;

use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::Proxy;
use wayland_client::QueueHandle;
use wayland_client::WEnum;

use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_v2::ZwpTabletPadV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::ZwpTabletSeatV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::ZwpTabletToolV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_v2::ZwpTabletV2;

use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_group_v2::{
    EVT_RING_OPCODE, EVT_STRIP_OPCODE,
};
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_v2::EVT_GROUP_OPCODE;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE;

use super::{CursorFocus, short_click};
use crate::OutputId;
use crate::draw::{DrawState, Point, ToolOverride};
use crate::state::State;

const EVDEV_STYLUS: u32 = 331;
const EVDEV_STYLUS2: u32 = 332;
const PEN: u8 = 1;
const BUTTON: u8 = 2;

#[derive(Default)]
pub(in crate::state) struct TabletState {
    tools: HashMap<ZwpTabletToolV2, ToolState>,
    gesture_owner: Option<ZwpTabletToolV2>,
    cursor_tool: Option<ZwpTabletToolV2>,
}

#[derive(Default)]
struct ToolState {
    frame: ToolFrame,
    focus: CursorFocus,
    eraser: bool,
    pen_held: bool,
    buttons_held: u8,
    suppressed_gesture: bool,
    button_press_time: Option<u32>,
}

impl ToolState {
    fn clear_gesture(&mut self) {
        self.pen_held = false;
        self.buttons_held = 0;
        self.suppress_gesture();
    }

    /// Ignores held input until it is released.
    fn suppress_gesture(&mut self) {
        self.suppressed_gesture = self.pen_held || self.buttons_held != 0;
        self.button_press_time = None;
    }
}

impl TabletState {
    pub(super) fn show_cursor(&mut self, draw: &mut DrawState) -> Option<bool> {
        let id = self.gesture_owner.as_ref().or(self.cursor_tool.as_ref())?;
        let tool = self
            .tools
            .get_mut(id)
            .filter(|tool| tool.focus.serial.is_some() && tool.focus.output.is_some())?;
        let tool_override =
            ToolOverride::from_eraser(tool.eraser || (tool.buttons_held != 0 && tool.pen_held));
        tool.focus.show(draw, tool_override, None, |serial| {
            id.set_cursor(serial, None, 0, 0)
        })
    }

    pub(super) fn clear_cursor_tool(&mut self) {
        self.cursor_tool = None;
    }

    pub(in crate::state) fn input_grab_active(&self) -> bool {
        self.gesture_owner.is_some()
    }

    /// Ends the gesture. Suppression keeps held input so its release drains.
    pub(super) fn cancel_gesture(&mut self, suppress: bool) {
        self.gesture_owner = None;
        for tool in self.tools.values_mut() {
            tool.frame.pressed = 0;
            tool.frame.released = 0;
            if suppress {
                tool.suppress_gesture();
            } else {
                tool.clear_gesture();
            }
        }
    }

    pub(in crate::state) fn restore_cursors(&mut self) {
        for tool in self.tools.values_mut() {
            tool.focus.restore();
        }
    }

    pub(in crate::state) fn remove_output(&mut self, output: OutputId) -> Option<Point> {
        let mut end_position = None;
        for (id, tool) in &mut self.tools {
            if tool.focus.output != Some(output) {
                continue;
            }
            if self.gesture_owner.as_ref() == Some(id) {
                end_position = tool.focus.position;
                self.gesture_owner = None;
            }
            if self.cursor_tool.as_ref() == Some(id) {
                self.cursor_tool = None;
            }
            tool.frame = ToolFrame::default();
            tool.focus.clear();
            tool.clear_gesture();
        }
        end_position
    }
}

impl Dispatch<ZwpTabletSeatV2, (), State> for State {
    fn event(
        state: &mut State,
        _tablet_seat: &ZwpTabletSeatV2,
        event: <ZwpTabletSeatV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qhandle: &QueueHandle<State>,
    ) {
        use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::Event;
        if let Event::ToolAdded { id } = event {
            let device = state
                .wayland
                .cursor_shape_manager
                .as_ref()
                .map(|manager| manager.get_tablet_tool_v2(&id, qhandle, ()));
            let mut tool = ToolState::default();
            tool.focus.device = device;
            state.tablet.tools.insert(id, tool);
        }
    }

    wayland_client::event_created_child!(State, ZwpTabletSeatV2, [
        EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<ZwpTabletV2, (), State> for State {
    fn event(
        _state: &mut State,
        tablet: &ZwpTabletV2,
        event: <ZwpTabletV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<State>,
    ) {
        if let wayland_protocols::wp::tablet::zv2::client::zwp_tablet_v2::Event::Removed = event {
            tablet.destroy();
        }
    }
}

impl Dispatch<ZwpTabletPadV2, (), State> for State {
    fn event(
        _state: &mut State,
        pad: &ZwpTabletPadV2,
        event: <ZwpTabletPadV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<State>,
    ) {
        use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_v2::Event;
        // Pad input is unused; receive child announcements before releasing it.
        if let Event::Done = event {
            pad.destroy();
        }
    }

    wayland_client::event_created_child!(State, ZwpTabletPadV2, [
        EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadGroupV2, (), State> for State {
    fn event(
        _state: &mut State,
        group: &ZwpTabletPadGroupV2,
        event: <ZwpTabletPadGroupV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<State>,
    ) {
        use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_group_v2::Event;
        match event {
            Event::Ring { ring } => ring.destroy(),
            Event::Strip { strip } => strip.destroy(),
            Event::Done => group.destroy(),
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, ZwpTabletPadGroupV2, [
        EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
    ]);
}

impl Dispatch<ZwpTabletToolV2, (), State> for State {
    fn event(
        state: &mut State,
        tablet_tool: &ZwpTabletToolV2,
        event: <ZwpTabletToolV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<State>,
    ) {
        use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::{Event, Type};
        if (!state.is_active() || state.freeze.capturing())
            && matches!(event, Event::Down { .. } | Event::Up | Event::Button { .. })
        {
            return;
        }
        if let Event::Removed = event {
            let tool = state.tablet.tools.remove(tablet_tool).unwrap_or_default();
            if state.tablet.gesture_owner.as_ref() == Some(tablet_tool) {
                state.tablet.gesture_owner = None;
                state.finish_interrupted_gesture(tool.focus.position);
            }
            if let Some(device) = tool.focus.device {
                device.destroy();
            }
            if state.tablet.cursor_tool.as_ref() == Some(tablet_tool) {
                state.tablet.cursor_tool = None;
            }
            tablet_tool.destroy();
            state.refresh_cursor();
            state.update_output_input();
            return;
        }
        let entered = match &event {
            Event::ProximityIn {
                serial, surface, ..
            } => Some((state.output_for_surface(surface), *serial)),
            _ => None,
        };
        let active = state.is_active();
        let Some(tool) = state.tablet.tools.get_mut(tablet_tool) else {
            return;
        };
        if let Some((output, serial)) = entered {
            tool.focus.enter(output, serial);
        }
        if let Event::Type { tool_type } = &event {
            tool.eraser = matches!(tool_type, WEnum::Value(Type::Eraser));
        }
        let output = tool.focus.output;
        let origin = output
            .and_then(|output| state.wayland.outputs.get(&output))
            .map_or_else(Point::default, |output| output.origin);
        let Some(frame) = tool.frame.dispatch(event, origin, &mut tool.buttons_held) else {
            return;
        };
        if let Some(pos) = frame.motion {
            tool.focus.position = Some(pos);
        }
        tool.pen_held = (tool.pen_held || frame.pressed(PEN)) && !frame.released(PEN);
        if frame.proximity_out {
            tool.focus.leave();
        }
        if !active || state.freeze.capturing() || output.is_none() {
            tool.clear_gesture();
            return;
        }
        let (pen_pressed, pen_released) = (frame.pressed(PEN), frame.released(PEN));
        let (button_pressed, button_released) = (frame.pressed(BUTTON), frame.released(BUTTON));
        if button_pressed {
            tool.button_press_time = Some(frame.time);
        }
        let short_button_click =
            button_released && short_click(tool.button_press_time.take(), Some(frame.time));
        let (eraser, pos, pen_held) = (tool.eraser, tool.focus.position, tool.pen_held);
        let button_held = tool.buttons_held != 0;
        let suppressed = state.pointer.input_grab_active()
            || tool.suppressed_gesture
            || state
                .tablet
                .gesture_owner
                .as_ref()
                .is_some_and(|owner| owner != tablet_tool);
        if frame.proximity_out {
            tool.clear_gesture();
        } else if suppressed {
            tool.suppress_gesture();
        }
        if suppressed {
            return;
        }
        if let Some(output) = output {
            state.focus_output(output);
        }
        // Selecting the first output can start an activation-time capture.
        if state.freeze.capturing() {
            state.tablet.cancel_gesture(false);
            return;
        }
        let tablet = &mut state.tablet;
        if (pen_pressed || button_pressed) && pos.is_some() {
            tablet.gesture_owner = Some(tablet_tool.clone());
        }
        if !frame.proximity_out {
            tablet.cursor_tool = Some(tablet_tool.clone());
        } else if tablet.cursor_tool.as_ref() == Some(tablet_tool) {
            tablet.cursor_tool = None;
        }
        let modifiers = state.keyboard.modifiers();
        if state.tablet.gesture_owner.as_ref() == Some(tablet_tool)
            && let Some(pos) = pos
        {
            if button_pressed && pen_held {
                state.pointer_up(pos, modifiers, false);
                state.pointer_down(pos, modifiers, ToolOverride::Eraser);
            } else if button_pressed {
                state.toggle_picker(pos);
            } else if pen_pressed {
                if eraser {
                    state.dismiss_picker();
                }
                let tool_override = ToolOverride::from_eraser(eraser || button_held);
                state.pointer_down(pos, modifiers, tool_override);
            }
            if button_released && pen_held && !pen_released {
                state.pointer_up(pos, modifiers, false);
                state.pointer_down(pos, modifiers, ToolOverride::from_eraser(eraser));
            } else if button_released && state.draw.editor().picker_active() {
                state.pointer_up(pos, modifiers, short_button_click);
            }
            if !button_pressed
                && !button_released
                && frame.motion.is_some()
                && (pen_held || (state.draw.editor().picker_active() && button_held))
            {
                state.pointer_motion(pos, modifiers);
            }
            if pen_released || frame.proximity_out {
                state.pointer_up(pos, modifiers, false);
            }
        }
        if frame.proximity_out || (!pen_held && !button_held) {
            state.tablet.gesture_owner = None;
        }
        state.refresh_cursor();
        if (frame.pressed | frame.released) != 0 || frame.proximity_out {
            state.update_output_input();
        }
    }
}

/// Tool events accumulated until their frame.
#[derive(Default)]
struct ToolFrame {
    motion: Option<Point>,
    pressed: u8,
    released: u8,
    proximity_out: bool,
    time: u32,
}

impl ToolFrame {
    fn pressed(&self, input: u8) -> bool {
        self.pressed & input != 0
    }

    fn released(&self, input: u8) -> bool {
        self.released & input != 0
    }

    fn dispatch(
        &mut self,
        event: <ZwpTabletToolV2 as Proxy>::Event,
        origin: Point,
        buttons_held: &mut u8,
    ) -> Option<Self> {
        use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::{ButtonState, Event};
        match event {
            Event::ProximityOut => self.proximity_out = true,
            Event::Down { .. } => self.pressed |= PEN,
            Event::Up => self.released |= PEN,
            Event::Motion { x, y } => self.motion = Some(Point::new(x as f32, y as f32) + origin),
            Event::Button { button, state, .. } => {
                let mask = match button {
                    EVDEV_STYLUS => 1,
                    EVDEV_STYLUS2 => 2,
                    _ => return None,
                };
                let was_held = *buttons_held != 0;
                match state {
                    WEnum::Value(ButtonState::Pressed) => *buttons_held |= mask,
                    WEnum::Value(ButtonState::Released) => *buttons_held &= !mask,
                    _ => return None,
                }
                if !was_held && *buttons_held != 0 {
                    // A release and another press in one frame keep the override held.
                    if self.released(BUTTON) {
                        self.released &= !BUTTON;
                    } else {
                        self.pressed |= BUTTON;
                    }
                } else if was_held && *buttons_held == 0 {
                    self.released |= BUTTON;
                }
            }
            Event::Frame { time } => {
                self.time = time;
                return Some(std::mem::take(self));
            }
            _ => {}
        }
        None
    }
}

use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::ZwpTabletManagerV2;

wayland_client::delegate_noop!(State: ignore ZwpTabletManagerV2);
wayland_client::delegate_noop!(State: ignore ZwpTabletPadRingV2);
wayland_client::delegate_noop!(State: ignore ZwpTabletPadStripV2);
