mod buffer;

use std::collections::BTreeMap;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_output::Transform;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_shm, wl_subsurface::WlSubsurface, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
    ext_image_copy_capture_manager_v1::Options,
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self, Flags, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use super::{DrawOn, OutputId, State};
use buffer::{DmabufBuffer, ShmBuffer, allocate_dmabuf};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct Freeze {
    pub on_activate: bool,
    pub defer_input: bool,
    generation: u64,
    phase: Phase,
}

enum Phase {
    Live,
    WaitingOutput,
    Capturing {
        deadline: Instant,
        frames: BTreeMap<OutputId, OutputCapture>,
    },
    Frozen,
}

#[derive(Default)]
struct Constraints {
    size: Option<(u32, u32)>,
    format: Option<wl_shm::Format>,
    device: Option<u64>,
    dmabuf_formats: Vec<(u32, Vec<u64>)>,
}

struct OutputCapture {
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    pending: Constraints,
    constraints: Option<Constraints>,
    revision: u64,
    hidden: bool,
    presented: bool,
    use_dmabuf: bool,
    state: OutputState,
}

enum OutputState {
    Idle,
    Allocating(PendingDmabuf),
    Capturing(CaptureFrame),
    Ready,
}

struct PendingDmabuf {
    params: ZwpLinuxBufferParamsV1,
    data: Option<DmabufBuffer>,
    revision: u64,
}

impl Drop for PendingDmabuf {
    fn drop(&mut self) {
        self.params.destroy();
    }
}

struct CaptureFrame {
    proxy: ExtImageCopyCaptureFrameV1,
    buffer: CaptureBuffer,
    transform: Transform,
}

impl Drop for CaptureFrame {
    fn drop(&mut self) {
        self.proxy.destroy();
    }
}

enum Storage {
    Shm(ShmBuffer),
    Dmabuf(DmabufBuffer),
}

struct CaptureBuffer {
    proxy: Option<WlBuffer>,
    storage: Storage,
}

impl Drop for CaptureBuffer {
    fn drop(&mut self) {
        if let Some(proxy) = self.proxy.take() {
            proxy.destroy();
        }
    }
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        // Cancel pending frames before closing their capture session.
        self.state = OutputState::Idle;
        self.session.destroy();
        self.source.destroy();
    }
}

impl Freeze {
    pub fn new(on_activate: bool) -> Self {
        Self {
            on_activate,
            defer_input: false,
            generation: 0,
            phase: Phase::Live,
        }
    }

    pub fn capturing(&self) -> bool {
        matches!(self.phase, Phase::Capturing { .. })
    }

    pub fn hides(&self, output: OutputId) -> bool {
        matches!(&self.phase, Phase::Capturing { frames, .. } if frames.get(&output).is_some_and(|f| f.hidden && !matches!(f.state, OutputState::Ready)))
    }

    pub fn next_wakeup(&self) -> Option<Instant> {
        match &self.phase {
            Phase::Capturing { deadline, .. } => Some(*deadline),
            _ => None,
        }
    }

    fn output_mut(&mut self, generation: u64, output: OutputId) -> Option<&mut OutputCapture> {
        if generation != self.generation {
            return None;
        }
        match &mut self.phase {
            Phase::Capturing { frames, .. } => frames.get_mut(&output),
            _ => None,
        }
    }
}

impl State {
    pub(super) fn toggle_freeze(&mut self) {
        if self.mode == super::Mode::Whiteboard {
            return;
        }
        if !self.freeze.capturing()
            && (self.draw.is_editing_text()
                || self.draw.picker_active()
                || self.pointer.input_grab_active()
                || self.tablet.input_grab_active())
        {
            return;
        }
        if !matches!(self.freeze.phase, Phase::Live) {
            self.stop_freeze(false);
        } else {
            self.start_freeze();
        }
    }

    pub(super) fn start_freeze(&mut self) {
        if self.draw_on == DrawOn::Current
            && self.selected_output.is_none()
            && !self.freeze.defer_input
        {
            self.freeze.phase = Phase::WaitingOutput;
            return;
        }
        let (Some(image_copy), Some(image_source)) = (
            &self.wayland.image_copy_manager,
            &self.wayland.image_capture_source_manager,
        ) else {
            self.fail_freeze(
                "Screen freeze unavailable: compositor does not support ext-image-copy-capture",
            );
            return;
        };
        let mut outputs = Vec::new();
        for (&id, output) in &self.wayland.outputs {
            // Capture before taking focus, then retain the output selected on entry.
            if self.draw_on == DrawOn::Current
                && self.selected_output.is_some_and(|selected| selected != id)
            {
                continue;
            }
            if output.wgpu.is_none() {
                self.fail_freeze("Screen freeze unavailable: output is not ready");
                return;
            }
            outputs.push(id);
        }
        if outputs.is_empty() {
            self.fail_freeze("Screen freeze unavailable: no output");
            return;
        }
        self.freeze.generation = self.freeze.generation.wrapping_add(1);
        let generation = self.freeze.generation;
        let use_dmabuf = self.wayland.dmabuf.is_some()
            && self.wayland.subcompositor.is_some()
            && self.wayland.viewporter.is_some();
        let frames = outputs
            .into_iter()
            .map(|id| {
                let source = image_source.create_source(
                    &self.wayland.outputs[&id].output,
                    &self.qhandle,
                    (),
                );
                let session = image_copy.create_session(
                    &source,
                    Options::empty(),
                    &self.qhandle,
                    (generation, id),
                );
                (
                    id,
                    OutputCapture {
                        source,
                        session,
                        pending: Constraints::default(),
                        constraints: None,
                        revision: 0,
                        hidden: false,
                        presented: false,
                        use_dmabuf,
                        state: OutputState::Idle,
                    },
                )
            })
            .collect();
        self.freeze.phase = Phase::Capturing {
            deadline: Instant::now() + CAPTURE_TIMEOUT,
            frames,
        };
        self.keyboard.cancel_repeat();
    }

    pub(super) fn freeze_output_selected(&mut self) {
        if matches!(self.freeze.phase, Phase::WaitingOutput) && self.selected_output.is_some() {
            self.start_freeze();
        } else if matches!(self.freeze.phase, Phase::Frozen) && self.draw_on == DrawOn::Current {
            for (&id, output) in &mut self.wayland.outputs {
                if self.selected_output != Some(id) && output.is_frozen() {
                    output.clear_frozen_background();
                    self.draw.damage(id);
                }
            }
            self.request_render();
        }
    }

    pub(super) fn stop_freeze(&mut self, deactivating: bool) {
        let deferred_input = std::mem::take(&mut self.freeze.defer_input);
        let old_phase = std::mem::replace(&mut self.freeze.phase, Phase::Live);
        let mut damaged = Vec::new();
        for (&id, output) in &mut self.wayland.outputs {
            if output.is_frozen()
                || matches!(&old_phase, Phase::Capturing { frames, .. } if frames.contains_key(&id))
            {
                output.clear_frozen_background();
                self.draw.damage(id);
                damaged.push(id);
            }
        }
        drop(old_phase);
        if !deactivating {
            if deferred_input {
                self.update_output_input();
            }
            for id in damaged {
                self.render(id);
            }
            self.request_render();
        }
    }

    pub(super) fn invalidate_freeze(&mut self) {
        if matches!(self.freeze.phase, Phase::Capturing { .. } | Phase::Frozen) {
            self.stop_freeze(false);
        }
    }

    pub(super) fn fail_freeze(&mut self, message: &str) {
        self.stop_freeze(false);
        eprintln!("vellum: {message}");
    }

    fn blank_output(&mut self, id: OutputId) -> Result<(), String> {
        let output = &self.wayland.outputs[&id];
        let blank = output
            .wgpu
            .as_ref()
            .unwrap()
            .hide_annotations()?
            .ok_or("could not hide annotations for capture")?;
        output
            .surface
            .frame(&self.qhandle, (self.freeze.generation, id));
        blank.present();
        self.freeze
            .output_mut(self.freeze.generation, id)
            .unwrap()
            .hidden = true;
        self.draw.damage(id);
        Ok(())
    }

    fn capture_output(&mut self, generation: u64, id: OutputId) -> Result<(), String> {
        let Some(capture) = self.freeze.output_mut(generation, id) else {
            return Ok(());
        };
        if !capture.presented || !matches!(capture.state, OutputState::Idle) {
            return Ok(());
        }
        let Some(constraints) = &capture.constraints else {
            return Ok(());
        };
        let (width, height) = constraints.size.ok_or("capture size unavailable")?;
        if capture.use_dmabuf
            && let Some(data) = constraints.device.and_then(|device| {
                allocate_dmabuf(device, width, height, &constraints.dmabuf_formats)
            })
        {
            let params = self
                .wayland
                .dmabuf
                .as_ref()
                .unwrap()
                .create_params(&self.qhandle, (generation, id));
            for (index, plane) in data.planes.iter().enumerate() {
                params.add(
                    plane.fd.as_fd(),
                    index as u32,
                    plane.offset,
                    plane.stride,
                    (data.modifier >> 32) as u32,
                    data.modifier as u32,
                );
            }
            params.create(width as i32, height as i32, data.format, Flags::empty());
            capture.state = OutputState::Allocating(PendingDmabuf {
                params,
                data: Some(data),
                revision: capture.revision,
            });
            return Ok(());
        }
        let format = constraints
            .format
            .ok_or("no supported capture buffer format")?;
        let shm = self.wayland.shm.as_ref().ok_or("wl_shm unavailable")?;
        let data = ShmBuffer::new(width, height, format)?;
        let proxy = data.create_buffer(shm, &self.qhandle);
        capture.attach(
            CaptureBuffer {
                proxy: Some(proxy),
                storage: Storage::Shm(data),
            },
            &self.qhandle,
            (generation, id),
        );
        Ok(())
    }

    fn finish_capture(&mut self, id: OutputId, mut frame: CaptureFrame) -> Result<(), String> {
        let output = self.wayland.outputs.get_mut(&id).unwrap();
        match &mut frame.buffer.storage {
            Storage::Shm(data) => {
                let size = [data.width, data.height];
                let is_bgra = data.is_bgra;
                let pixels = data.pixel_data();
                output.wgpu.as_mut().unwrap().set_frozen_background(
                    size,
                    &pixels,
                    is_bgra,
                    frame.transform,
                )?;
                log::debug!("frozen output {id}: shared memory {}x{}", size[0], size[1]);
            }
            Storage::Dmabuf(data) => {
                if output.frozen_surface.is_none() {
                    let surface = self.wayland.compositor.create_surface(&self.qhandle, id);
                    let subsurface = self.wayland.subcompositor.as_ref().unwrap().get_subsurface(
                        &surface,
                        &output.surface,
                        &self.qhandle,
                        (),
                    );
                    subsurface.place_below(&output.surface);
                    let viewport = self.wayland.viewporter.as_ref().unwrap().get_viewport(
                        &surface,
                        &self.qhandle,
                        (),
                    );
                    let empty = self.wayland.compositor.create_region(&self.qhandle, ());
                    surface.set_input_region(Some(&empty));
                    empty.destroy();
                    output.frozen_surface = Some(FrozenSurface {
                        surface,
                        subsurface,
                        viewport,
                        buffer: None,
                    });
                }
                let frozen = output.frozen_surface.as_mut().unwrap();
                frozen
                    .viewport
                    .set_destination(output.logical_size[0] as i32, output.logical_size[1] as i32);
                frozen.surface.set_buffer_transform(frame.transform);
                let buffer = frame.buffer.proxy.take().unwrap();
                frozen.surface.attach(Some(&buffer), 0, 0);
                frozen.surface.damage(0, 0, i32::MAX, i32::MAX);
                // Present the frozen desktop together with its parent annotations.
                frozen.surface.commit();
                frozen.buffer = Some(buffer);
                log::debug!(
                    "frozen output {id}: DMA-BUF {}x{}, {} planes, modifier {:#x}",
                    data.width,
                    data.height,
                    data.planes.len(),
                    data.modifier
                );
            }
        }
        self.draw.damage(id);
        self.render(id);
        self.handle_freeze(Instant::now());
        Ok(())
    }

    pub(super) fn handle_freeze(&mut self, now: Instant) {
        let Phase::Capturing { deadline, frames } = &self.freeze.phase else {
            return;
        };
        if frames
            .values()
            .all(|f| matches!(f.state, OutputState::Ready))
        {
            log::debug!(
                "screen frozen in {:?}",
                now.duration_since(*deadline - CAPTURE_TIMEOUT)
            );
            self.freeze.phase = Phase::Frozen;
            if self.selected_output.is_some() {
                self.freeze_output_selected();
            }
            if std::mem::take(&mut self.freeze.defer_input) {
                self.update_output_input();
            }
            self.request_render();
        } else if now >= *deadline {
            self.fail_freeze("Screen freeze timed out; returned to live drawing");
        }
    }
}

impl OutputCapture {
    fn attach(&mut self, buffer: CaptureBuffer, qh: &QueueHandle<State>, data: (u64, OutputId)) {
        let frame = self.session.create_frame(qh, data);
        frame.attach_buffer(buffer.proxy.as_ref().unwrap());
        let (width, height) = match &buffer.storage {
            Storage::Shm(data) => (data.width, data.height),
            Storage::Dmabuf(data) => (data.width, data.height),
        };
        frame.damage_buffer(0, 0, width as i32, height as i32);
        frame.capture();
        self.state = OutputState::Capturing(CaptureFrame {
            proxy: frame,
            buffer,
            transform: Transform::Normal,
        });
    }
}

fn format_score(format: wl_shm::Format) -> u8 {
    match format {
        wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => 3,
        wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => 2,
        wl_shm::Format::Rgb888 | wl_shm::Format::Bgr888 => 1,
        _ => 0,
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, (u64, OutputId)> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        &(generation, id): &(u64, OutputId),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(capture) = state.freeze.output_mut(generation, id) else {
            return;
        };
        if matches!(capture.state, OutputState::Ready) {
            return;
        }
        use ext_image_copy_capture_session_v1::Event;
        let result = match event {
            Event::BufferSize { width, height } => {
                capture.pending.size = Some((width, height));
                Ok(())
            }
            Event::ShmFormat {
                format: WEnum::Value(format),
            } => {
                if format_score(format) > capture.pending.format.map_or(0, format_score) {
                    capture.pending.format = Some(format);
                }
                Ok(())
            }
            Event::DmabufDevice { device } => {
                capture.pending.device = device.as_slice().try_into().ok().map(u64::from_ne_bytes);
                Ok(())
            }
            Event::DmabufFormat { format, modifiers } => {
                if modifiers.len() % 8 == 0 {
                    capture.pending.dmabuf_formats.push((
                        format,
                        modifiers
                            .as_chunks::<8>()
                            .0
                            .iter()
                            .copied()
                            .map(u64::from_ne_bytes)
                            .collect(),
                    ));
                }
                Ok(())
            }
            Event::Done => {
                let constraints = std::mem::take(&mut capture.pending);
                if !constraints.size.is_some_and(|(w, h)| {
                    w > 0 && h > 0 && i32::try_from(w).is_ok() && i32::try_from(h).is_ok()
                }) {
                    Err("invalid capture dimensions".into())
                } else if constraints.format.is_none()
                    && !(capture.use_dmabuf
                        && constraints.device.is_some()
                        && !constraints.dmabuf_formats.is_empty())
                {
                    Err("no supported capture buffer format".into())
                } else {
                    capture.constraints = Some(constraints);
                    capture.revision += 1;
                    if !capture.hidden {
                        state.blank_output(id)
                    } else {
                        state.capture_output(generation, id)
                    }
                }
            }
            Event::Stopped => Err("capture session stopped by compositor".into()),
            _ => Ok(()),
        };
        if let Err(error) = result {
            state.fail_freeze(&format!("Screen freeze failed: {error}"));
        }
    }
}

impl Dispatch<WlCallback, (u64, OutputId)> for State {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        _: <WlCallback as Proxy>::Event,
        &(generation, id): &(u64, OutputId),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(capture) = state.freeze.output_mut(generation, id) else {
            return;
        };
        capture.presented = true;
        if let Err(error) = state.capture_output(generation, id) {
            state.fail_freeze(&format!("Screen freeze failed: {error}"));
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, (u64, OutputId)> for State {
    fn event(
        state: &mut Self,
        proxy: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        &(generation, id): &(u64, OutputId),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(capture) = state.freeze.output_mut(generation, id) else {
            return;
        };
        let OutputState::Capturing(frame) = &mut capture.state else {
            return;
        };
        if frame.proxy != *proxy {
            return;
        }
        use ext_image_copy_capture_frame_v1::Event;
        let result = match event {
            Event::Transform {
                transform: WEnum::Value(transform),
            } => {
                frame.transform = transform;
                Ok(())
            }
            Event::Ready => {
                let OutputState::Capturing(frame) =
                    std::mem::replace(&mut capture.state, OutputState::Ready)
                else {
                    unreachable!()
                };
                state.finish_capture(id, frame)
            }
            Event::Failed {
                reason: WEnum::Value(FailureReason::BufferConstraints),
            } => {
                capture.state = OutputState::Idle;
                // Reallocate using the latest complete batch. A failed buffer
                // does not require the compositor to send new constraints.
                state.capture_output(generation, id)
            }
            Event::Failed { reason } => Err(format!("frame capture error ({reason:?})")),
            _ => Ok(()),
        };
        if let Err(error) = result {
            state.fail_freeze(&format!("Screen freeze failed: {error}"));
        }
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, (u64, OutputId)> for State {
    fn event(
        state: &mut Self,
        proxy: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        &(generation, id): &(u64, OutputId),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let current = state.freeze.output_mut(generation, id).is_some_and(|capture| matches!(&capture.state, OutputState::Allocating(pending) if pending.params == *proxy));
        if !current {
            if let zwp_linux_buffer_params_v1::Event::Created { buffer } = event {
                buffer.destroy();
            }
            return;
        }
        let capture = state.freeze.output_mut(generation, id).unwrap();
        let OutputState::Allocating(mut pending) =
            std::mem::replace(&mut capture.state, OutputState::Idle)
        else {
            unreachable!()
        };
        let revision = pending.revision;
        let data = pending.data.take().unwrap();
        drop(pending);
        match event {
            zwp_linux_buffer_params_v1::Event::Created { buffer }
                if revision == capture.revision =>
            {
                capture.attach(
                    CaptureBuffer {
                        proxy: Some(buffer),
                        storage: Storage::Dmabuf(data),
                    },
                    &state.qhandle,
                    (generation, id),
                );
                return;
            }
            zwp_linux_buffer_params_v1::Event::Created { buffer } => buffer.destroy(),
            zwp_linux_buffer_params_v1::Event::Failed => capture.use_dmabuf = false,
            _ => {}
        }
        if let Err(error) = state.capture_output(generation, id) {
            state.fail_freeze(&format!("Screen freeze failed: {error}"));
        }
    }
    wayland_client::event_created_child!(State, ZwpLinuxBufferParamsV1, [0 => (WlBuffer, ())]);
}

// The compositor samples the captured buffer beneath the transparent annotation
// surface. Vellum never maps it or imports it into its Vulkan device.
pub(super) struct FrozenSurface {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    buffer: Option<WlBuffer>,
}

impl FrozenSurface {
    pub(super) fn is_frozen(&self) -> bool {
        self.buffer.is_some()
    }

    pub(super) fn clear(&mut self) {
        // Apply removal with the replacement parent frame, including when its
        // presentation is delayed. Reuse the empty subsurface on the next freeze.
        self.surface.attach(None, 0, 0);
        self.surface.commit();
        if let Some(buffer) = self.buffer.take() {
            buffer.destroy();
        }
    }
}

impl Drop for FrozenSurface {
    fn drop(&mut self) {
        self.subsurface.destroy();
        self.viewport.destroy();
        self.surface.destroy();
        if let Some(buffer) = self.buffer.take() {
            buffer.destroy();
        }
    }
}

wayland_client::delegate_noop!(State: ignore WlBuffer);
wayland_client::delegate_noop!(State: ignore WlSubsurface);
