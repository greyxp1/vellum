//! Frame callbacks, damage scheduling and presentation.

use std::time::{Duration, Instant};

use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use super::{State, output::Output};
use crate::OutputId;
use crate::render::GpuContext;

// Acquisition may fail on an occluded surface. Back off without relying on a
// frame callback that the compositor may withhold until the surface is visible.
const RENDER_RETRY_INTERVAL: Duration = Duration::from_millis(100);

impl State {
    pub(super) fn render(&mut self, output: OutputId) {
        if self.fatal_error.is_some() || self.freeze.hides(output) {
            return;
        }
        let presentation = self.presentation(output);
        if let Some(output_state) = self.wayland.outputs.get_mut(&output) {
            log::trace!("rendering output {output} ({:?})", output_state.name);
            let scale = output_state.render_scale();
            let Some(wgpu) = output_state.wgpu.as_mut() else {
                return;
            };
            let text_input = &mut self.text_input;
            let proxy = self.wayland.text_input.as_ref();
            let surface = &output_state.surface;
            let frame_pending = &mut output_state.frame_pending;
            let commit_pending = &mut output_state.commit_pending;
            let qhandle = &self.qhandle;
            if let Err(error) = self.draw.render(
                output,
                output_state.origin,
                scale,
                presentation,
                wgpu,
                |snapshot| {
                    text_input.sync_render(proxy, output, snapshot);
                    *commit_pending = false;
                    // Request the callback only once acquisition and rendering
                    // succeeded, so Vulkan commits it with the new buffer.
                    if !*frame_pending {
                        surface.frame(qhandle, output);
                        *frame_pending = true;
                    }
                },
            ) {
                self.fatal_error = Some(error);
                return;
            }
            output_state.render_retry = self
                .draw
                .needs_render(output)
                .then(|| Instant::now() + RENDER_RETRY_INTERVAL);
        }
    }

    pub(super) fn request_render(&mut self) {
        self.render_requested = true;
    }

    pub(crate) fn render_pending(&mut self) {
        if let Some(error) = self.gpu.as_ref().and_then(GpuContext::lost) {
            self.fatal_error = Some(error.clone());
            return;
        }
        if !std::mem::take(&mut self.render_requested) {
            return;
        }
        let now = Instant::now();
        let can_render = |output: &Output| {
            output.wgpu.is_some()
                && match output.render_retry {
                    Some(deadline) => now >= deadline,
                    None => !output.frame_pending || output.commit_pending,
                }
        };
        // Advance stroke samples when the input output can present, including
        // retries while an earlier frame callback is withheld.
        if self
            .input_output
            .filter(|id| !self.freeze.hides(*id))
            .and_then(|id| self.wayland.outputs.get(&id))
            .is_some_and(can_render)
        {
            self.flush_pen_motion();
        }
        let outputs: Vec<_> = self
            .draw
            .damaged_outputs()
            .filter(|id| {
                !self.freeze.hides(*id) && self.wayland.outputs.get(id).is_some_and(can_render)
            })
            .collect();
        for id in outputs {
            self.render(id);
        }
    }
}

impl Dispatch<WlCallback, OutputId> for State {
    fn event(
        state: &mut Self,
        _callback: &WlCallback,
        event: <WlCallback as Proxy>::Event,
        output: &OutputId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_callback::Event;
        if let Event::Done { callback_data: _ } = event {
            if let Some(output_state) = state.wayland.outputs.get_mut(output) {
                output_state.frame_pending = false;
            }
            state.request_render();
        }
    }
}
