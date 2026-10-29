use std::collections::HashMap;

use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::Proxy;
use wayland_client::QueueHandle;
use wayland_client::WEnum;

use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};

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

use super::pointer::update_cursor;
use super::short_click;
use crate::OutputId;
use crate::draw::{Cursor, DrawState, Point, ToolOverride};
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
    event_sequence: EventSequence,
    cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    cursor_serial: Option<u32>,
    current_cursor: Option<Cursor>,
    eraser: bool,
    output: Option<OutputId>,
    pos: Option<Point>,
    pen_held: bool,
    buttons_held: u8,
    suppressed_gesture: bool,
    button_press_time: Option<u32>,
}

impl ToolState {
    fn clear_gesture(&mut self) {
        self.pen_held = false;
        self.buttons_held = 0;
        self.suppressed_gesture = false;
        self.button_press_time = None;
    }

    fn refresh_cursor(&mut self, tablet_tool: &ZwpTabletToolV2, cursor: Cursor) {
        update_cursor(
            self.cursor_serial,
            self.cursor_shape_device.as_ref(),
            &mut self.current_cursor,
            cursor,
            |serial| tablet_tool.set_cursor(serial, None, 0, 0),
        );
    }
}

impl TabletState {
    pub(in crate::state) fn refresh_cursor(&mut self, draw: &mut DrawState) -> Option<bool> {
        let tablet_tool = self.gesture_owner.as_ref().or(self.cursor_tool.as_ref())?;
        let tool = self
            .tools
            .get_mut(tablet_tool)
            .filter(|tool| tool.cursor_serial.is_some() && tool.output.is_some())?;
        let point = tool.pos?;
        let tool_override =
            ToolOverride::from_eraser(tool.eraser || (tool.buttons_held != 0 && tool.pen_held));
        let cursor = draw.cursor(point, tool_override);
        let changed = draw.set_tool_cursor(match cursor.tool() {
            Some(preview) if tool.cursor_shape_device.is_some() => Some((point, preview)),
            _ => None,
        });
        tool.refresh_cursor(tablet_tool, cursor);
        Some(changed)
    }

    pub(in crate::state) fn clear_cursor_tool(&mut self) {
        self.cursor_tool = None;
    }

    pub(in crate::state) fn input_grab_active(&self) -> bool {
        self.gesture_owner.is_some()
    }

    pub(in crate::state) fn cancel_gesture(&mut self) {
        self.gesture_owner = None;
        for tool in self.tools.values_mut() {
            tool.event_sequence.pressed = 0;
            tool.event_sequence.released = 0;
            tool.clear_gesture();
        }
    }

    pub(in crate::state) fn restore_cursors(&mut self) {
        for tool in self.tools.values_mut() {
            if let (Some(serial), Some(device)) = (tool.cursor_serial, &tool.cursor_shape_device) {
                device.set_shape(serial, Shape::Default);
            }
            tool.current_cursor = None;
        }
    }

    pub(in crate::state) fn suppress_gestures(&mut self) {
        self.gesture_owner = None;
        for tool in self.tools.values_mut() {
            tool.event_sequence.pressed = 0;
            tool.event_sequence.released = 0;
            tool.suppressed_gesture = tool.pen_held || tool.buttons_held != 0;
            tool.button_press_time = None;
        }
    }

    pub(in crate::state) fn remove_output(&mut self, output: OutputId) -> Option<Point> {
        let mut end_position = None;
        for (id, tool) in &mut self.tools {
            if tool.output != Some(output) {
                continue;
            }
            if self.gesture_owner.as_ref() == Some(id) {
                end_position = tool.pos;
                self.gesture_owner = None;
            }
            if self.cursor_tool.as_ref() == Some(id) {
                self.cursor_tool = None;
            }
            tool.event_sequence = EventSequence::default();
            tool.output = None;
            tool.pos = None;
            tool.cursor_serial = None;
            tool.current_cursor = None;
            tool.clear_gesture();
        }
        end_position
    }
}

impl Dispatch<ZwpTabletSeatV2, (), State> for TabletState {
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
            let cursor_shape_device = state
                .wayland
                .cursor_shape_manager
                .as_ref()
                .map(|manager| manager.get_tablet_tool_v2(&id, qhandle, ()));
            state.tablet.tools.insert(
                id,
                ToolState {
                    cursor_shape_device,
                    ..Default::default()
                },
            );
        }
    }

    wayland_client::event_created_child!(State, ZwpTabletSeatV2, [
        EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<ZwpTabletV2, (), State> for TabletState {
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

impl Dispatch<ZwpTabletPadV2, (), State> for TabletState {
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

impl Dispatch<ZwpTabletPadGroupV2, (), State> for TabletState {
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

impl Dispatch<ZwpTabletToolV2, (), State> for TabletState {
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
            let tool = state.tablet.tools.remove(tablet_tool);
            if state.tablet.gesture_owner.as_ref() == Some(tablet_tool) {
                state.tablet.gesture_owner = None;
                state.finish_interrupted_gesture(tool.as_ref().and_then(|tool| tool.pos));
            }
            if let Some(device) = tool.and_then(|tool| tool.cursor_shape_device) {
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
        let proximity_output = match &event {
            Event::ProximityIn { surface, .. } => Some(state.output_for_surface(surface)),
            _ => None,
        };
        let active = state.is_active();
        let Some(tool) = state.tablet.tools.get_mut(tablet_tool) else {
            return;
        };
        if let Some(output) = proximity_output {
            tool.output = output;
        }
        if let Event::Type { tool_type } = &event {
            tool.eraser = matches!(tool_type, WEnum::Value(Type::Eraser));
        }
        let output = tool.output;
        let origin = output
            .and_then(|output| state.wayland.outputs.get(&output))
            .map_or_else(Point::default, |output| output.origin);
        let Some(sequence) = tool
            .event_sequence
            .dispatch(event, origin, &mut tool.buttons_held)
        else {
            return;
        };
        if let Some(pos) = sequence.motion {
            tool.pos = Some(pos);
        }
        if sequence.pressed(PEN) {
            tool.pen_held = true;
        }
        if sequence.released(PEN) {
            tool.pen_held = false;
        }
        if let Some(serial) = sequence.enter_serial {
            tool.cursor_serial = Some(serial);
            tool.current_cursor = None;
        }
        if sequence.proximity_out {
            tool.cursor_serial = None;
            tool.current_cursor = None;
            tool.output = None;
        }
        if !active || state.freeze.capturing() || output.is_none() {
            tool.clear_gesture();
            return;
        }
        let pen_pressed = sequence.pressed(PEN);
        let pen_released = sequence.released(PEN);
        let button_pressed = sequence.pressed(BUTTON);
        let button_released = sequence.released(BUTTON);
        let eraser = tool.eraser;
        if button_pressed {
            tool.button_press_time = Some(sequence.time);
        }
        let short_button_click =
            button_released && short_click(tool.button_press_time.take(), Some(sequence.time));
        let pos = tool.pos;
        let pen_held = tool.pen_held;
        let button_held = tool.buttons_held != 0;
        if sequence.proximity_out {
            tool.pen_held = false;
            tool.buttons_held = 0;
            tool.button_press_time = None;
        }
        if state.pointer.input_grab_active()
            || tool.suppressed_gesture
            || state
                .tablet
                .gesture_owner
                .as_ref()
                .is_some_and(|owner| owner != tablet_tool)
        {
            tool.suppressed_gesture = !sequence.proximity_out && (pen_held || button_held);
            tool.button_press_time = None;
            return;
        }
        if let Some(output) = output {
            state.focus_output(output);
        }
        // Selecting the first output can start an activation-time capture.
        if state.freeze.capturing() {
            state.tablet.cancel_gesture();
            return;
        }
        if (pen_pressed || button_pressed) && pos.is_some() {
            state.tablet.gesture_owner = Some(tablet_tool.clone());
        }
        if !sequence.proximity_out {
            state.tablet.cursor_tool = Some(tablet_tool.clone());
        } else if state.tablet.cursor_tool.as_ref() == Some(tablet_tool) {
            state.tablet.cursor_tool = None;
        }
        let modifiers = state.modifiers();
        if state.tablet.gesture_owner.as_ref() == Some(tablet_tool) {
            if button_pressed && let Some(pos) = pos {
                if pen_held {
                    state.pointer_up(pos, modifiers, false);
                    state.pointer_down(pos, modifiers, ToolOverride::Eraser);
                } else {
                    state.toggle_picker(pos);
                }
            }
            if pen_pressed
                && !button_pressed
                && let Some(pos) = pos
            {
                if eraser {
                    state.dismiss_picker();
                }
                state.pointer_down(
                    pos,
                    modifiers,
                    ToolOverride::from_eraser(eraser || button_held),
                );
            }
            if button_released
                && pen_held
                && !pen_released
                && let Some(pos) = pos
            {
                state.pointer_up(pos, modifiers, false);
                state.pointer_down(pos, modifiers, ToolOverride::from_eraser(eraser));
            } else if button_released
                && state.draw.picker_active()
                && let Some(pos) = pos
            {
                state.pointer_up(pos, modifiers, short_button_click);
            }
            if !button_pressed
                && !button_released
                && sequence.motion.is_some()
                && (pen_held || (state.draw.picker_active() && button_held))
                && let Some(pos) = pos
            {
                state.pointer_motion(pos, modifiers);
            }
            if pen_released && let Some(pos) = pos {
                state.pointer_up(pos, modifiers, false);
            }
        }
        if sequence.proximity_out
            && !pen_released
            && state.tablet.gesture_owner.as_ref() == Some(tablet_tool)
            && let Some(pos) = pos
        {
            state.pointer_up(pos, modifiers, false);
        }
        if sequence.proximity_out || (!pen_held && !button_held) {
            state.tablet.gesture_owner = None;
        }
        state.refresh_cursor();
        if (sequence.pressed | sequence.released) != 0 || sequence.proximity_out {
            state.update_output_input();
        }
    }
}

#[derive(Default, Clone, Copy)]
struct EventSequence {
    motion: Option<Point>,

    pressed: u8,
    released: u8,

    enter_serial: Option<u32>,
    proximity_out: bool,
    time: u32,
}

impl EventSequence {
    fn pressed(self, input: u8) -> bool {
        self.pressed & input != 0
    }

    fn released(self, input: u8) -> bool {
        self.released & input != 0
    }

    fn dispatch(
        &mut self,
        event: <ZwpTabletToolV2 as Proxy>::Event,
        origin: Point,
        buttons_held: &mut u8,
    ) -> Option<Self> {
        use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::Event;
        match event {
            Event::ProximityIn {
                serial,
                tablet: _,
                surface: _,
            } => {
                self.enter_serial = Some(serial);
            }
            Event::ProximityOut => {
                self.proximity_out = true;
            }
            Event::Down { serial: _ } => {
                self.pressed |= PEN;
            }
            Event::Up => {
                self.released |= PEN;
            }
            Event::Motion { x, y } => {
                self.motion = Some(Point::new(x as f32, y as f32) + origin);
            }
            Event::Button {
                serial: _,
                button,
                state: button_state,
            } => {
                use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::ButtonState;
                let mask = match button {
                    EVDEV_STYLUS => 1,
                    EVDEV_STYLUS2 => 2,
                    _ => return None,
                };
                let was_held = *buttons_held != 0;
                match button_state {
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
wayland_client::delegate_dispatch!(State: [ZwpTabletSeatV2: ()] => TabletState);
wayland_client::delegate_dispatch!(State: [ZwpTabletV2: ()] => TabletState);
wayland_client::delegate_dispatch!(State: [ZwpTabletToolV2: ()] => TabletState);
wayland_client::delegate_dispatch!(State: [ZwpTabletPadV2: ()] => TabletState);
wayland_client::delegate_dispatch!(State: [ZwpTabletPadGroupV2: ()] => TabletState);
wayland_client::delegate_noop!(State: ignore ZwpTabletPadRingV2);
wayland_client::delegate_noop!(State: ignore ZwpTabletPadStripV2);
