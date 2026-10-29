//! Output surfaces, scaling, hotplug and layer-shell configuration.

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::ZxdgOutputV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

use super::State;
use crate::OutputId;
use crate::draw::Point;
use crate::render::{GpuContext, WgpuState};

pub(super) struct Output {
    pub(super) output: WlOutput,
    pub(super) name: String,
    pub(super) xdg_output: Option<ZxdgOutputV1>,
    pub(super) origin: Point,
    pub(super) logical_size: [u32; 2],
    pub(super) integer_scale: i32,
    pub(super) preferred_scale: Option<u32>,
    pub(super) fractional_scale: Option<WpFractionalScaleV1>,
    pub(super) viewport: Option<WpViewport>,
    pub(super) surface: WlSurface,
    pub(super) layer_surface: ZwlrLayerSurfaceV1,
    pub(super) frame_pending: bool,
    pub(super) commit_pending: bool,
    pub(super) input_state: (bool, bool),
    pub(super) render_retry: Option<std::time::Instant>,
    pub(super) wgpu: Option<WgpuState>,
    pub(super) frozen_surface: Option<super::freeze::FrozenSurface>,
    pub(super) transform: u32,
}

impl Output {
    pub(super) fn is_frozen(&self) -> bool {
        self.frozen_surface
            .as_ref()
            .is_some_and(super::freeze::FrozenSurface::is_frozen)
    }

    pub(super) fn clear_frozen_background(&mut self) {
        if let Some(surface) = &mut self.frozen_surface {
            surface.clear();
        }
    }

    fn buffer_size(&self) -> [u32; 2] {
        let scale = self
            .preferred_scale
            .map_or(f64::from(self.integer_scale), |scale| {
                f64::from(scale) / 120.0
            });
        self.logical_size
            .map(|size| (f64::from(size) * scale).round() as u32)
    }

    pub(super) fn render_scale(&self) -> [f64; 2] {
        let size = self.buffer_size();
        [
            f64::from(size[0]) / f64::from(self.logical_size[0]),
            f64::from(size[1]) / f64::from(self.logical_size[1]),
        ]
    }

    fn configure_scale(&self) {
        if let Some(viewport) = &self.viewport {
            self.surface.set_buffer_scale(1);
            viewport.set_destination(self.logical_size[0] as i32, self.logical_size[1] as i32);
        } else {
            self.surface.set_buffer_scale(self.integer_scale);
        }
    }
}

impl State {
    pub(super) fn add_outputs(&mut self) {
        let globals = self
            .wayland
            .registry
            .data::<GlobalListContents>()
            .unwrap()
            .clone_list();
        for global in globals {
            if global.interface == WlOutput::interface().name {
                self.add_output(global.name, global.version);
            }
        }
    }

    pub(super) fn add_output(&mut self, id: OutputId, version: u32) {
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

        if self.wayland.outputs.contains_key(&id) {
            return;
        }
        self.invalidate_freeze();
        let output =
            self.wayland
                .registry
                .bind::<WlOutput, _, _>(id, version.min(4), &self.qhandle, id);
        let xdg_output = self
            .wayland
            .xdg_output_manager
            .as_ref()
            .map(|manager| manager.get_xdg_output(&output, &self.qhandle, id));
        let surface = self.wayland.compositor.create_surface(&self.qhandle, id);
        let fractional_scale = self
            .wayland
            .fractional_scale_manager
            .as_ref()
            .map(|manager| manager.get_fractional_scale(&surface, &self.qhandle, id));
        let viewport = fractional_scale.as_ref().map(|_| {
            self.wayland
                .viewporter
                .as_ref()
                .unwrap()
                .get_viewport(&surface, &self.qhandle, ())
        });
        let layer_surface = self.wayland.layer_shell.get_layer_surface(
            &surface,
            Some(&output),
            Layer::Overlay,
            "vellum".into(),
            &self.qhandle,
            id,
        );
        layer_surface.set_anchor(Anchor::all());
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_exclusive_zone(-1);
        let empty_region = self.wayland.compositor.create_region(&self.qhandle, ());
        surface.set_input_region(Some(&empty_region));
        empty_region.destroy();
        surface.commit();

        self.wayland.outputs.insert(
            id,
            Output {
                output,
                name: String::new(),
                xdg_output,
                origin: Point::default(),
                logical_size: [0; 2],
                integer_scale: 1,
                preferred_scale: None,
                fractional_scale,
                viewport,
                surface,
                layer_surface,
                frame_pending: false,
                commit_pending: false,
                input_state: (false, false),
                render_retry: None,
                wgpu: None,
                frozen_surface: None,
                transform: 0,
            },
        );
        self.draw.add_output(id);
        if self.keyboard_output.is_none() {
            self.keyboard_output = Some(id);
        }
        if self.is_active() {
            self.update_output_input();
        }
    }

    pub(super) fn remove_output(&mut self, id: OutputId) {
        let Some(mut output) = self.wayland.outputs.remove(&id) else {
            return;
        };
        if self.selected_output == Some(id) {
            self.pending_pen_motion.reset(None);
            self.keyboard.cancel_repeat();
            self.pointer.suppress_gesture();
            self.tablet.suppress_gestures();
            self.draw.deactivate();
            self.sync_text_input();
        }
        let pointer_position = self.pointer.remove_output(id);
        let tablet_position = self.tablet.remove_output(id);
        if let Some(pos) = tablet_position.or(pointer_position) {
            self.finish_interrupted_gesture(Some(pos));
        }
        self.invalidate_freeze();
        output.frozen_surface.take();
        output.wgpu.take();
        if let Some(scale) = output.fractional_scale {
            scale.destroy();
        }
        if let Some(viewport) = output.viewport {
            viewport.destroy();
        }
        if let Some(xdg_output) = output.xdg_output {
            xdg_output.destroy();
        }
        output.layer_surface.destroy();
        output.surface.destroy();
        if output.output.version() >= 3 {
            output.output.release();
        }
        self.draw.remove_output(id);
        self.text_input_output_removed(id);

        if self.selected_output == Some(id) {
            self.selected_output = None;
            self.damage_presentation();
        }
        if self.input_output == Some(id) {
            self.input_output = None;
        }
        if self.keyboard_output == Some(id) {
            self.keyboard_output = self.wayland.outputs.keys().next().copied();
        }
        if self.is_active() {
            self.update_output_input();
        }
        self.refresh_cursor();
    }

    pub(super) fn output_for_surface(&self, surface: &WlSurface) -> Option<OutputId> {
        surface
            .data::<OutputId>()
            .copied()
            .filter(|output| self.wayland.outputs.contains_key(output))
    }

    pub(super) fn output_origin(&self, output: OutputId) -> Point {
        self.wayland
            .outputs
            .get(&output)
            .map_or_else(Point::default, |output| output.origin)
    }

    fn set_output_origin(&mut self, output: OutputId, origin: Point) {
        let Some(output_state) = self.wayland.outputs.get_mut(&output) else {
            return;
        };
        if output_state.origin == origin {
            return;
        }
        output_state.origin = origin;
        self.invalidate_freeze();
        self.draw.damage(output);
        self.request_render();
    }

    fn resize_output(&mut self, id: OutputId) {
        let Some(output) = self.wayland.outputs.get_mut(&id) else {
            return;
        };
        if output.logical_size.contains(&0) {
            return;
        }
        output.configure_scale();
        let [width, height] = output.buffer_size();
        if let Some(wgpu) = &mut output.wgpu {
            if let Err(error) = wgpu.resize(width, height) {
                self.fatal_error = Some(error);
                return;
            }
            self.draw.damage(id);
            self.request_render();
        }
    }
}

impl Dispatch<WlOutput, OutputId> for State {
    fn event(
        state: &mut Self,
        _proxy: &WlOutput,
        event: <WlOutput as Proxy>::Event,
        output: &OutputId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_output::Event;
        let Some(output_state) = state.wayland.outputs.get_mut(output) else {
            return;
        };
        let changed = match event {
            Event::Name { ref name } => {
                output_state.name.clone_from(name);
                false
            }
            Event::Geometry { transform, .. } => {
                let transform = u32::from(transform);
                std::mem::replace(&mut output_state.transform, transform) != transform
            }
            // A preferred fractional scale overrides the integer scale.
            Event::Scale { factor } => {
                std::mem::replace(&mut output_state.integer_scale, factor.max(1)) != factor.max(1)
                    && output_state.preferred_scale.is_none()
            }
            _ => false,
        };
        if changed && matches!(event, Event::Scale { .. }) {
            state.resize_output(*output);
        }
        if changed {
            state.invalidate_freeze();
        }
        if let Event::Geometry { x, y, .. } = event
            && state
                .wayland
                .outputs
                .get(output)
                .is_some_and(|output| output.xdg_output.is_none())
        {
            state.set_output_origin(*output, Point::new(x as f32, y as f32));
        }
    }
}

impl Dispatch<WpFractionalScaleV1, OutputId> for State {
    fn event(
        state: &mut Self,
        _proxy: &WpFractionalScaleV1,
        event: <WpFractionalScaleV1 as Proxy>::Event,
        output: &OutputId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let Some(output_state) = state.wayland.outputs.get_mut(output) else { return; };
            if output_state.preferred_scale.replace(scale.max(1)) != Some(scale.max(1)) {
                state.resize_output(*output);
                state.invalidate_freeze();
            }
        }
    }
}

impl Dispatch<ZxdgOutputV1, OutputId> for State {
    fn event(
        state: &mut Self,
        _proxy: &ZxdgOutputV1,
        event: <ZxdgOutputV1 as Proxy>::Event,
        output: &OutputId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::Event;
        if let Event::Name { ref name } = event
            && let Some(output) = state.wayland.outputs.get_mut(output)
        {
            output.name.clone_from(name);
        }
        if let Event::LogicalPosition { x, y } = event {
            state.set_output_origin(*output, Point::new(x as f32, y as f32));
        }
    }
}

impl Dispatch<WlSurface, OutputId> for State {
    fn event(
        _state: &mut Self,
        _proxy: &WlSurface,
        _event: <WlSurface as Proxy>::Event,
        _data: &OutputId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ignore WpViewport);
impl Dispatch<WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _callback: &WlCallback,
        _event: <WlCallback as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        state.add_outputs();
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, OutputId> for State {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        output: &OutputId,
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
                if width == 0 || height == 0 {
                    return;
                }
                let Some(output_state) = state.wayland.outputs.get_mut(output) else {
                    return;
                };
                let resized = output_state.logical_size != [width, height];
                output_state.logical_size = [width, height];
                if output_state.wgpu.is_some() {
                    // Input/focus changes can produce another configure with
                    // unchanged dimensions. Its acknowledgement needs no redraw.
                    if resized {
                        // Resize first so stopping a freeze renders at the new size.
                        state.resize_output(*output);
                        state.invalidate_freeze();
                    }
                } else {
                    output_state.configure_scale();
                    let [width, height] = output_state.buffer_size();
                    let surface = output_state.surface.clone();
                    let display = state.wayland.display.clone();
                    let wgpu = if let Some(gpu) = &state.gpu {
                        gpu.create_surface(&display, &surface)
                            .and_then(|surface| WgpuState::new(gpu, surface, width, height))
                    } else {
                        GpuContext::new(&display, &surface, width, height).map(|(gpu, wgpu)| {
                            state.gpu = Some(gpu);
                            wgpu
                        })
                    };
                    match wgpu {
                        Ok(wgpu) => output_state.wgpu = Some(wgpu),
                        Err(error) => {
                            state.fatal_error = Some(error);
                            return;
                        }
                    }

                    // Some compositors require a buffer with the initial configure.
                    state.render(*output);
                }
            }
            Event::Closed => {
                state.remove_output(*output);
                // A closed layer surface does not necessarily mean its output is gone.
                // Wait for accompanying registry removals before recreating surfaces.
                state.wayland.display.sync(&state.qhandle, ());
            }
            _ => {}
        }
    }
}
