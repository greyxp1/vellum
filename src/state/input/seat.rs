use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

use crate::state::State;

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: <WlSeat as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_seat::Capability;
        use wayland_client::protocol::wl_seat::Event;
        let Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        if capabilities.contains(Capability::Pointer) && state.wayland.pointer.is_none() {
            let pointer = seat.get_pointer(qhandle, ());
            let shape_device = state
                .wayland
                .cursor_shape_manager
                .as_ref()
                .map(|manager| manager.get_pointer(&pointer, qhandle, ()));
            state.pointer.set_cursor_device(shape_device);
            state.wayland.pointer = Some(pointer);
        } else if !capabilities.contains(Capability::Pointer)
            && let Some(pointer) = state.wayland.pointer.take()
        {
            state.interrupt_pointer_gesture();
            pointer.release();
            state.pointer.clear_pointer();
            state.refresh_cursor();
            state.update_output_input();
        }
        if capabilities.contains(Capability::Keyboard) && state.wayland.keyboard.is_none() {
            state.wayland.keyboard = Some(seat.get_keyboard(qhandle, ()));
        } else if !capabilities.contains(Capability::Keyboard)
            && let Some(keyboard) = state.wayland.keyboard.take()
        {
            state.release_keyboard_gestures();
            state.keyboard.clear();
            keyboard.release();
        }
    }
}
