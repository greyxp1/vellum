//! Wayland connection, globals and registry events.

use std::collections::BTreeMap;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_display::WlDisplay;
use wayland_client::protocol::wl_keyboard::WlKeyboard;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::{wl_shm::WlShm, wl_shm_pool::WlShmPool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::ZwpTabletManagerV2;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::ZwpTextInputV3;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

use super::input::PendingPenMotion;
use super::output::Output;
use super::{State, freeze, input};
use crate::OutputId;
use crate::draw;

pub(super) struct WaylandState {
    pub(super) _connection: Connection,
    pub(super) display: WlDisplay,
    pub(super) registry: WlRegistry,
    pub(super) compositor: WlCompositor,
    pub(super) subcompositor: Option<WlSubcompositor>,
    pub(super) dmabuf: Option<ZwpLinuxDmabufV1>,
    pub(super) shm: Option<WlShm>,
    pub(super) image_copy_manager: Option<ExtImageCopyCaptureManagerV1>,
    pub(super) image_capture_source_manager: Option<ExtOutputImageCaptureSourceManagerV1>,
    _seat: WlSeat,
    pub(super) layer_shell: ZwlrLayerShellV1,
    pub(super) outputs: BTreeMap<OutputId, Output>,
    pub(super) pointer: Option<WlPointer>,
    pub(super) keyboard: Option<WlKeyboard>,
    pub(super) text_input: Option<ZwpTextInputV3>,

    pub(super) cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    pub(super) xdg_output_manager: Option<ZxdgOutputManagerV1>,
    pub(super) viewporter: Option<WpViewporter>,
    pub(super) fractional_scale_manager: Option<WpFractionalScaleManagerV1>,
}

impl State {
    pub fn setup_wayland(
        settings: &crate::config::Settings,
    ) -> Result<(Self, EventQueue<Self>), String> {
        let connection = Connection::connect_to_env()
            .map_err(|error| format!("could not connect to Wayland: {error}"))?;
        let (globals, event_queue) = registry_queue_init::<State>(&connection)
            .map_err(|error| format!("Wayland setup failed: {error}"))?;
        let qhandle = event_queue.handle();
        let display = connection.display();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&qhandle, 3..=6, ())
            .map_err(|_| "compositor does not provide wl_compositor version 3 or newer")?;
        let seat = globals
            .bind::<WlSeat, _, _>(&qhandle, 5..=9, ())
            .map_err(|_| "compositor does not provide wl_seat version 5 or newer")?;
        let layer_shell = globals
            .bind::<ZwlrLayerShellV1, _, _>(&qhandle, 1..=4, ())
            .map_err(|_| "compositor does not provide zwlr_layer_shell_v1")?;
        let cursor_shape_manager = globals
            .bind::<WpCursorShapeManagerV1, _, _>(&qhandle, 1..=1, ())
            .ok();
        let mut tablet = input::TabletState::default();
        if let Ok(manager) = globals.bind::<ZwpTabletManagerV2, _, _>(&qhandle, 1..=1, ()) {
            tablet.set_tablet_seat(manager.get_tablet_seat(&seat, &qhandle, ()));
            manager.destroy();
        }
        let xdg_output_manager = globals
            .bind::<ZxdgOutputManagerV1, _, _>(&qhandle, 1..=3, ())
            .ok();
        let viewporter = globals.bind::<WpViewporter, _, _>(&qhandle, 1..=1, ()).ok();
        let subcompositor = globals
            .bind::<WlSubcompositor, _, _>(&qhandle, 1..=1, ())
            .ok();
        let dmabuf = globals
            .bind::<ZwpLinuxDmabufV1, _, _>(&qhandle, 3..=3, ())
            .ok();
        let shm = globals.bind::<WlShm, _, _>(&qhandle, 1..=1, ()).ok();
        let image_copy_manager = globals
            .bind::<ExtImageCopyCaptureManagerV1, _, _>(&qhandle, 1..=1, ())
            .ok();
        let image_capture_source_manager = globals
            .bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(&qhandle, 1..=1, ())
            .ok();
        let fractional_scale_manager = viewporter.as_ref().and_then(|_| {
            globals
                .bind::<WpFractionalScaleManagerV1, _, _>(&qhandle, 1..=1, ())
                .ok()
        });
        let text_input = globals
            .bind::<ZwpTextInputManagerV3, _, _>(&qhandle, 1..=2, ())
            .ok()
            .map(|manager| {
                let text_input = manager.get_text_input(&seat, &qhandle, ());
                manager.destroy();
                text_input
            });
        let output_globals = globals.contents().clone_list();
        let annotations_visible = settings.annotations_visible;
        let whiteboard_color = settings.whiteboard_color;
        let draw_on = settings.draw_on;
        let clear_on_cancel = settings.clear_on_cancel;
        let freeze = freeze::Freeze::new(settings.freeze_on_activate);
        let keyboard =
            input::KeyboardState::new(settings.keybinds.clone(), settings.text_keybinds.clone());

        let mut state = Self {
            fatal_error: None,
            mode: super::Mode::Inactive,
            annotations_visible,
            whiteboard_color,
            draw_on,
            selected_output: None,
            input_output: None,
            keyboard_output: None,
            clear_on_cancel,
            freeze,
            pending_pen_motion: PendingPenMotion::default(),
            render_requested: false,
            wayland: WaylandState {
                _connection: connection,
                display,
                registry: globals.registry().clone(),
                compositor,
                subcompositor,
                dmabuf,
                shm,
                image_copy_manager,
                image_capture_source_manager,
                _seat: seat,
                layer_shell,
                outputs: BTreeMap::new(),
                pointer: None,
                keyboard: None,
                text_input,
                cursor_shape_manager,
                xdg_output_manager,
                viewporter,
                fractional_scale_manager,
            },
            draw: draw::DrawState::new(settings),
            keyboard,
            text_input: input::TextInputState::default(),
            pointer: input::PointerState::default(),
            tablet,
            gpu: None,
            qhandle,
        };

        for global in output_globals {
            if global.interface == WlOutput::interface().name {
                state.add_output(global.name, global.version);
            }
        }

        Ok((state, event_queue))
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        _registry: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_registry::Event;
        match event {
            Event::Global {
                name,
                interface,
                version,
            } if interface == WlOutput::interface().name => state.add_output(name, version),
            Event::GlobalRemove { name } => state.remove_output(name),
            _ => {}
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlRegion);
delegate_noop!(State: ignore ZwpTextInputManagerV3);
delegate_noop!(State: ignore WpViewporter);
delegate_noop!(State: ignore WpFractionalScaleManagerV1);
delegate_noop!(State: ignore ZxdgOutputManagerV1);
delegate_noop!(State: ignore ZwlrLayerShellV1);
delegate_noop!(State: ignore WpCursorShapeManagerV1);

delegate_noop!(State: ignore WlSubcompositor);
delegate_noop!(State: ignore ZwpLinuxDmabufV1);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore ExtImageCaptureSourceV1);
delegate_noop!(State: ignore ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ignore ExtOutputImageCaptureSourceManagerV1);
