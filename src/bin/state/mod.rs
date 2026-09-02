mod draw;
mod input;

pub(crate) use draw::Tool;

use color::{ColorSpaceTag, DynamicColor};
use wayland_client::delegate_dispatch;

use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::EventQueue;
use wayland_client::Proxy;
use wayland_client::QueueHandle;
use wayland_client::WEnum;

use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_display::WlDisplay;
use wayland_client::protocol::wl_keyboard::WlKeyboard;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;

use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;

use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::ZwpTabletManagerV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_v2::ZwpTabletPadV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::ZwpTabletSeatV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::ZwpTabletToolV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_v2::ZwpTabletV2;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

use crate::render::WgpuState;
use draw::{Action, Cursor, Modifiers, Point};

const MAX_PENDING_PEN_SAMPLES: usize = 64;
const PEN_BEND_DEVIATION_SQUARED: f32 = 0.5 * 0.5;

macro_rules! delegate_noop {
    ($proxy:ty) => {
        impl Dispatch<$proxy, ()> for State {
            fn event(
                _state: &mut Self,
                _proxy: &$proxy,
                _event: <$proxy as Proxy>::Event,
                _data: &(),
                _conn: &Connection,
                _qhandle: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

#[derive(Default)]
struct SetupWaylandState {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,

    layer_shell: Option<ZwlrLayerShellV1>,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    tablet_manager: Option<ZwpTabletManagerV2>,
}

impl SetupWaylandState {
    fn into_state(
        self,
        connection: Connection,
        display: WlDisplay,
        qhandle: &QueueHandle<State>,
    ) -> Result<WaylandState, &'static str> {
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

        let compositor = self
            .compositor
            .ok_or("compositor does not provide wl_compositor")?;
        let seat = self.seat.ok_or("compositor does not provide wl_seat")?;
        let layer_shell = self
            .layer_shell
            .ok_or("compositor does not provide zwlr_layer_shell_v1")?;
        let surface = compositor.create_surface(qhandle, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            Layer::Overlay,
            "vellum".into(),
            qhandle,
            (),
        );
        layer_surface.set_anchor(Anchor::all());
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_exclusive_zone(-1);

        Ok(WaylandState {
            _connection: connection,
            display,
            compositor,
            surface,
            seat,
            layer_surface,
            pointer: None,
            keyboard: None,
            cursor_shape_manager: self.cursor_shape_manager,
            tablet_manager: self.tablet_manager,
        })
    }
}

impl Dispatch<WlRegistry, QueueHandle<State>> for SetupWaylandState {
    fn event(
        setup_state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
        state_qhandle: &QueueHandle<State>,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_registry::Event;
        match event {
            Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => {
                    let compositor = registry.bind::<WlCompositor, _, _>(
                        name,
                        version.min(5),
                        state_qhandle,
                        (),
                    );
                    setup_state.compositor = Some(compositor);
                }
                "wl_seat" => {
                    let seat =
                        registry.bind::<WlSeat, _, _>(name, version.min(9), state_qhandle, ());
                    setup_state.seat = Some(seat);
                }
                "zwlr_layer_shell_v1" => {
                    let layer_shell = registry.bind::<ZwlrLayerShellV1, _, _>(
                        name,
                        version.min(4),
                        state_qhandle,
                        (),
                    );
                    setup_state.layer_shell = Some(layer_shell);
                }
                "wp_cursor_shape_manager_v1" => {
                    let cursor_shape_manager = registry.bind::<WpCursorShapeManagerV1, _, _>(
                        name,
                        version.min(1),
                        state_qhandle,
                        (),
                    );
                    setup_state.cursor_shape_manager = Some(cursor_shape_manager);
                }
                "zwp_tablet_manager_v2" => {
                    let tablet_manager = registry.bind::<ZwpTabletManagerV2, _, _>(
                        name,
                        version.min(1),
                        state_qhandle,
                        (),
                    );
                    setup_state.tablet_manager = Some(tablet_manager);
                }
                _ => {}
            },
            Event::GlobalRemove { name: _ } => {}
            _ => {}
        }
    }
}

#[derive(Default)]
struct PendingPenMotion {
    anchor: Option<Point>,
    samples: Vec<Point>,
}

impl PendingPenMotion {
    fn reset(&mut self, anchor: Option<Point>) {
        self.anchor = anchor;
        self.samples.clear();
    }

    fn push(&mut self, point: Point) {
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

    fn take(&mut self) -> ([Point; 2], usize) {
        let Some(&end) = self.samples.last() else {
            return ([Point::default(); 2], 0);
        };
        let mut output = [end; 2];
        let mut count = 1;
        if let Some(anchor) = self.anchor {
            let bend = self.samples[..self.samples.len() - 1]
                .iter()
                .copied()
                .map(|point| (point, segment_distance_squared(point, anchor, end)))
                .max_by(|(_, first), (_, second)| first.total_cmp(second));
            if let Some((bend, deviation)) = bend
                && deviation >= PEN_BEND_DEVIATION_SQUARED
                && bend != anchor
                && bend != end
            {
                output = [bend, end];
                count = 2;
            }
        }
        self.anchor = Some(end);
        self.samples.clear();
        (output, count)
    }
}

fn segment_distance_squared(point: Point, start: Point, end: Point) -> f32 {
    let segment_x = end.x - start.x;
    let segment_y = end.y - start.y;
    let length_squared = segment_x.powi(2) + segment_y.powi(2);
    if length_squared <= f32::EPSILON {
        return (point.x - start.x).powi(2) + (point.y - start.y).powi(2);
    }
    let projection = (((point.x - start.x) * segment_x + (point.y - start.y) * segment_y)
        / length_squared)
        .clamp(0.0, 1.0);
    let nearest_x = start.x + segment_x * projection;
    let nearest_y = start.y + segment_y * projection;
    (point.x - nearest_x).powi(2) + (point.y - nearest_y).powi(2)
}

pub struct State {
    active: bool,
    clear_on_escape: bool,
    frame_pending: bool,
    pending_pen_motion: PendingPenMotion,

    wayland: WaylandState,
    draw: draw::DrawState,
    keyboard: input::KeyboardState,
    pointer: input::PointerState,
    tablet: input::TabletState,

    wgpu: Option<WgpuState>,
    qhandle: QueueHandle<State>,
}

impl State {
    pub fn setup_wayland(settings: crate::Settings) -> Result<(Self, EventQueue<Self>), String> {
        let connection = Connection::connect_to_env()
            .map_err(|error| format!("could not connect to Wayland: {error}"))?;
        let mut setup_queue = connection.new_event_queue();
        let event_queue = connection.new_event_queue();

        let display = connection.display();
        let _registry = display.get_registry(&setup_queue.handle(), event_queue.handle());

        let mut tmp_wayland_state = SetupWaylandState::default();

        setup_queue
            .roundtrip(&mut tmp_wayland_state)
            .map_err(|error| format!("Wayland setup failed: {error}"))?;

        let qhandle = event_queue.handle();
        let wayland_state = tmp_wayland_state
            .into_state(connection, display, &qhandle)
            .map_err(str::to_string)?;
        wayland_state.surface.commit();

        let mut state = Self {
            active: false,
            clear_on_escape: settings.clear_on_escape,
            frame_pending: false,
            pending_pen_motion: PendingPenMotion::default(),
            wayland: wayland_state,
            draw: draw::DrawState::new(settings),
            keyboard: input::KeyboardState::default(),
            pointer: input::PointerState::default(),
            tablet: input::TabletState::default(),
            wgpu: None,
            qhandle,
        };

        if let Some(manager) = &state.wayland.tablet_manager {
            state.tablet.set_tablet_seat(manager.get_tablet_seat(
                &state.wayland.seat,
                &state.qhandle,
                (),
            ));
        }

        Ok((state, event_queue))
    }

    pub fn toggle_input(&mut self) {
        self.set_input_active(!self.active);
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_text_editing(&self) -> bool {
        self.draw.is_editing_text()
    }

    pub fn set_input_active(&mut self, active: bool) {
        if active == self.active {
            return;
        }
        if active {
            self.activate();
        } else {
            self.deactivate();
        }
    }

    pub fn activate(&mut self) {
        // reset to full region
        self.wayland
            .layer_surface
            .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        self.wayland.surface.set_input_region(None);
        self.wayland.surface.commit();

        self.active = true;
        if self.draw.activate() {
            self.request_render();
        }
    }

    pub fn deactivate(&mut self) {
        self.keyboard.cancel_repeat();
        let preview_changed = self.draw.deactivate();
        self.pointer.restore_cursor();
        self.tablet.restore_cursors();
        let empty_region = self.wayland.compositor.create_region(&self.qhandle, ());
        self.wayland
            .layer_surface
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        self.wayland.surface.set_input_region(Some(&empty_region));
        self.wayland.surface.commit();

        self.active = false;
        if preview_changed {
            self.request_render();
        } else if let Some(wgpu) = &mut self.wgpu {
            wgpu.release_picker_target();
        }
    }

    fn render(&mut self) {
        if let Some(wgpu) = &mut self.wgpu {
            self.draw.render(wgpu);
            if !self.active {
                wgpu.release_picker_target();
            }
        }
    }

    fn force_render(&mut self) {
        if let Some(wgpu) = &mut self.wgpu {
            self.draw.force_render(wgpu);
            if !self.active {
                wgpu.release_picker_target();
            }
        }
    }

    fn apply_action(&mut self, action: Action) {
        let clear_on_escape = self.clear_on_escape && matches!(action, Action::Cancel);
        let anchor = self
            .pointer
            .position()
            .map(|(x, y)| Point::new(x as f32, y as f32));
        let effect = self.draw.handle_action(action, anchor);
        if effect.damage.changed() {
            self.request_render();
        }
        if effect.deactivate {
            if clear_on_escape {
                self.clear();
            }
            self.deactivate();
        }
        self.refresh_pointer_cursor();
    }

    pub fn clear(&mut self) {
        self.apply_action(Action::Clear);
    }

    pub fn set_current_color(&mut self, color: DynamicColor) {
        let rgba = color.convert(ColorSpaceTag::Srgb).clip().components;
        let render = self.draw.set_current_color(rgba);
        if render {
            self.request_render();
        }
    }

    fn modifiers(&self) -> Modifiers {
        self.keyboard.modifiers()
    }

    fn modifiers_changed(&mut self) {
        let modifiers = self.modifiers();
        if self.draw.modifiers_changed(modifiers) {
            self.request_render();
        }
    }

    fn cancel_pointer_gesture(&mut self) {
        let interaction_active = self.pointer.cancel_gesture();
        self.pending_pen_motion.reset(None);
        if self.active && (interaction_active || self.draw.picker_active()) {
            self.apply_action(Action::Cancel);
        }
    }

    fn pointer_down(
        &mut self,
        (x, y): (f64, f64),
        modifiers: Modifiers,
        tool_override: draw::ToolOverride,
    ) {
        let point = Point::new(x as f32, y as f32);
        self.pending_pen_motion.reset(None);
        if self.draw.picker_active() {
            let changed = if tool_override == draw::ToolOverride::Eraser {
                self.draw.dismiss_picker()
            } else {
                self.draw.picker_motion(point)
            };
            if changed {
                self.request_render();
            }
            return;
        }
        let changed = self.draw.pointer_down(point, modifiers, tool_override);
        if self.draw.is_drawing_pen() {
            self.pending_pen_motion.reset(Some(point));
        }
        if changed {
            self.request_render();
        }
    }

    fn pointer_motion(&mut self, (x, y): (f64, f64), modifiers: Modifiers) {
        let point = Point::new(x as f32, y as f32);
        if self.draw.picker_active() {
            if self.draw.picker_motion(point) {
                self.request_render();
            }
            return;
        }
        // Preserve one real bend inside each display frame without letting a
        // high-polling-rate device grow the stroke without bound.
        if self.draw.is_drawing_pen() {
            self.pending_pen_motion.push(point);
            self.request_render();
            return;
        }
        if self.draw.pointer_motion(point, modifiers) {
            self.request_render();
        }
    }

    fn pointer_up(&mut self, (x, y): (f64, f64), modifiers: Modifiers, latch_picker: bool) {
        self.flush_pen_motion();
        let point = Point::new(x as f32, y as f32);
        if self.draw.picker_active() {
            if self.draw.picker_release(point, latch_picker) {
                self.request_render();
            }
            return;
        }
        if self.draw.pointer_up(point, modifiers) {
            self.request_render();
        }
        self.pending_pen_motion.reset(None);
    }

    fn open_picker(&mut self, (x, y): (f64, f64)) {
        if self.draw.open_picker(Point::new(x as f32, y as f32)) {
            self.request_render();
        }
    }

    fn toggle_picker(&mut self, pos: (f64, f64)) {
        if self.draw.picker_active() {
            self.dismiss_picker();
        } else {
            self.open_picker(pos);
        }
    }

    fn dismiss_picker(&mut self) {
        if self.draw.dismiss_picker() {
            self.request_render();
        }
    }

    fn double_click_at(&mut self, (x, y): (f64, f64)) -> bool {
        let changed = self.draw.double_click_at(Point::new(x as f32, y as f32));
        if changed {
            self.request_render();
        }
        changed
    }

    fn adjust(&mut self, steps: f32, (x, y): (f64, f64), modifiers: Modifiers) -> bool {
        let adjustment = self
            .draw
            .adjust(steps, Point::new(x as f32, y as f32), modifiers);
        if adjustment.changed {
            self.request_render();
        }
        adjustment.hit_stop
    }

    fn refresh_pointer_cursor(&mut self) {
        if !self.active {
            self.clear_tool_cursor();
            return;
        }
        if self.tablet.cursor_active() {
            return;
        }
        let (Some((x, y)), Some(pointer)) = (self.pointer.position(), &self.wayland.pointer) else {
            self.clear_tool_cursor();
            return;
        };
        let point = Point::new(x as f32, y as f32);
        let cursor = self.draw.cursor(point, self.pointer.tool_override());
        let preview_changed = self.draw.set_tool_cursor(match cursor {
            Cursor::Tool(preview) if self.pointer.tool_cursor_supported() => Some((point, preview)),
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

    fn request_render(&mut self) {
        if self.frame_pending {
            return;
        }
        self.flush_pen_motion();
        if self.wgpu.is_none() || !self.needs_frame() {
            return;
        }
        self.wayland.surface.frame(&self.qhandle, ());
        self.frame_pending = true;
        self.render();
        // A successful presentation commits the frame request with its buffer.
        // If acquisition failed, commit the callback alone so it can retry.
        if self.needs_frame() {
            self.wayland.surface.commit();
        }
    }

    fn flush_pen_motion(&mut self) {
        let (points, count) = self.pending_pen_motion.take();
        if count > 0 {
            let modifiers = self.modifiers();
            self.draw.pen_motion(&points[..count], modifiers);
        }
    }

    pub fn next_wakeup(&self) -> Option<std::time::Instant> {
        [self.draw.next_wakeup(), self.keyboard.next_wakeup()]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn handle_timeouts(&mut self, now: std::time::Instant) {
        if let Some(action) = self
            .keyboard
            .repeat_action(now, self.draw.is_editing_text())
        {
            self.apply_action(action);
        }
        if self.draw.handle_timeouts(now) {
            self.request_render();
        }
    }

    fn needs_frame(&self) -> bool {
        self.draw.needs_render()
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.wgpu.take();
    }
}

struct WaylandState {
    _connection: Connection,
    display: WlDisplay,
    compositor: WlCompositor,
    surface: WlSurface,
    seat: WlSeat,

    layer_surface: ZwlrLayerSurfaceV1,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,

    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    tablet_manager: Option<ZwpTabletManagerV2>,
}

delegate_noop!(WlCompositor);
delegate_noop!(WlRegion);
delegate_noop!(WlSurface);

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
            if pointer.version() >= 3 {
                pointer.release();
            }
            state.pointer.clear_pointer();
            state.refresh_pointer_cursor();
        }
        if capabilities.contains(Capability::Keyboard) && state.wayland.keyboard.is_none() {
            state.wayland.keyboard = Some(seat.get_keyboard(qhandle, ()));
        } else if !capabilities.contains(Capability::Keyboard)
            && let Some(keyboard) = state.wayland.keyboard.take()
        {
            state.keyboard.clear();
            if keyboard.version() >= 3 {
                keyboard.release();
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _callback: &WlCallback,
        event: <WlCallback as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_callback::Event;
        if let Event::Done { callback_data: _ } = event {
            state.frame_pending = false;
            state.flush_pen_motion();
            if state.needs_frame() {
                state.request_render();
            }
        }
    }
}

delegate_noop!(ZwlrLayerShellV1);
impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event;
        match event {
            Event::Configure {
                serial,
                width,
                height,
            } => {
                layer_surface.ack_configure(serial);

                if let Some(wgpu) = &mut state.wgpu {
                    wgpu.resize(width, height);
                    state.draw.damage_scene();
                    state.request_render();
                } else {
                    let wgpu = WgpuState::new(
                        &state.wayland.display,
                        &state.wayland.surface,
                        width,
                        height,
                    );

                    state.wgpu = Some(wgpu);

                    // some compositors are unhappy if we don't force render here
                    state.force_render();
                }
            }
            Event::Closed => {}
            _ => {}
        }
    }
}

delegate_dispatch!(State: [WlPointer: ()] => input::PointerState);

delegate_noop!(WpCursorShapeManagerV1);
delegate_noop!(WpCursorShapeDeviceV1);

delegate_noop!(ZwpTabletManagerV2);
delegate_dispatch!(State: [ZwpTabletSeatV2: ()] => input::TabletState);
delegate_noop!(ZwpTabletV2);
delegate_dispatch!(State: [ZwpTabletToolV2: ()] => input::TabletState);
delegate_noop!(ZwpTabletPadV2);
