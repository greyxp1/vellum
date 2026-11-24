mod buffer;

use std::collections::BTreeMap;
use std::os::fd::{AsFd, OwnedFd};
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

use super::wayland::WaylandState;
use super::{DrawOn, OutputId, State};
use buffer::{DmabufBuffer, allocate_dmabuf, create_shm_buffer};

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

// A batch is validated on its done event, so stored constraints always have
// usable dimensions and at least one buffer type.
#[derive(Default)]
struct Constraints {
    size: [u32; 2],
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
    failed_revision: u64,
    use_dmabuf: bool,
    step: Step,
}

enum Step {
    // Annotations are visible until the first constraints arrive.
    Live,
    // Annotations are hidden, waiting for the blank frame to be presented.
    Blanking,
    Idle,
    Allocating(PendingDmabuf),
    Capturing(CaptureFrame),
    Ready,
}

// Params are destroyed on their created or failed event so that a cancelled
// freeze still receives and destroys the compositor's buffer.
struct PendingDmabuf {
    params: ZwpLinuxBufferParamsV1,
    data: DmabufBuffer,
    revision: u64,
}

struct CaptureFrame {
    proxy: ExtImageCopyCaptureFrameV1,
    // Taken when the frame is ready, so the frame can be destroyed while frozen.
    buffer: Option<CaptureBuffer>,
    transform: Transform,
}

impl Drop for CaptureFrame {
    fn drop(&mut self) {
        self.proxy.destroy();
    }
}

// Retains the memory behind a buffer for as long as the compositor may use it.
enum Backing {
    Shm { _fd: OwnedFd },
    Dmabuf { _buffer: DmabufBuffer },
}

struct CaptureBuffer {
    proxy: WlBuffer,
    _backing: Backing,
}

impl Drop for CaptureBuffer {
    fn drop(&mut self) {
        self.proxy.destroy();
    }
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        // Cancel pending frames before closing their capture session.
        self.step = Step::Live;
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
        matches!(&self.phase, Phase::Capturing { frames, .. } if frames.get(&output).is_some_and(|f| !matches!(f.step, Step::Live | Step::Ready)))
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
        let wayland = &self.wayland;
        let (Some(image_copy), Some(image_source), Some(_), Some(_)) = (
            &wayland.image_copy_manager,
            &wayland.image_capture_source_manager,
            &wayland.subcompositor,
            &wayland.viewporter,
        ) else {
            return self.fail_freeze(
                "Screen freeze unavailable: compositor does not support ext-image-copy-capture, wl_subcompositor or wp_viewporter",
            );
        };
        // Capture before taking focus, then retain the output selected on entry.
        let outputs: Vec<_> = wayland
            .outputs
            .iter()
            .filter(|&(&id, _)| {
                self.draw_on != DrawOn::Current || self.selected_output.is_none_or(|s| s == id)
            })
            .collect();
        if outputs.is_empty() {
            return self.fail_freeze("Screen freeze unavailable: no output");
        }
        if outputs.iter().any(|(_, output)| output.wgpu.is_none()) {
            return self.fail_freeze("Screen freeze unavailable: output is not ready");
        }
        self.freeze.generation = self.freeze.generation.wrapping_add(1);
        let generation = self.freeze.generation;
        let frames = outputs
            .into_iter()
            .map(|(&id, output)| {
                let source = image_source.create_source(&output.output, &self.qhandle, ());
                let session = image_copy.create_session(
                    &source,
                    Options::empty(),
                    &self.qhandle,
                    (generation, id),
                );
                let capture = OutputCapture {
                    source,
                    session,
                    pending: Constraints::default(),
                    constraints: None,
                    revision: 0,
                    failed_revision: 0,
                    use_dmabuf: wayland.dmabuf.is_some(),
                    step: Step::Live,
                };
                (id, capture)
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
            let selected = self.selected_output;
            self.thaw_outputs(|id, frozen| frozen && selected != Some(id));
            self.request_render();
        }
    }

    pub(super) fn stop_freeze(&mut self, deactivating: bool) {
        let deferred_input = std::mem::take(&mut self.freeze.defer_input);
        let old_phase = std::mem::replace(&mut self.freeze.phase, Phase::Live);
        let thawed = self.thaw_outputs(|id, frozen| {
            frozen
                || matches!(&old_phase, Phase::Capturing { frames, .. } if frames.contains_key(&id))
        });
        drop(old_phase);
        if !deactivating {
            if deferred_input {
                self.update_output_input();
            }
            for id in thawed {
                self.render(id);
            }
            self.request_render();
        }
    }

    /// Detaches the frozen background of each matching output and damages it.
    fn thaw_outputs(&mut self, thaw: impl Fn(OutputId, bool) -> bool) -> Vec<OutputId> {
        let mut thawed = Vec::new();
        for (&id, output) in &mut self.wayland.outputs {
            let frozen = output
                .frozen_surface
                .as_mut()
                .filter(|f| f.buffer.is_some());
            if !thaw(id, frozen.is_some()) {
                continue;
            }
            if let Some(frozen) = frozen {
                // Apply removal with the replacement parent frame, including when its
                // presentation is delayed. Reuse the empty subsurface on the next freeze.
                frozen.surface.attach(None, 0, 0);
                frozen.surface.commit();
                frozen.buffer = None;
            }
            self.draw.damage(id);
            thawed.push(id);
        }
        thawed
    }

    pub(super) fn invalidate_freeze(&mut self) {
        if matches!(self.freeze.phase, Phase::Capturing { .. } | Phase::Frozen) {
            self.stop_freeze(false);
        }
    }

    fn fail_freeze(&mut self, message: &str) {
        self.stop_freeze(false);
        log::warn!("{message}");
    }

    /// Moves an output's capture to its next step once its prerequisites are met.
    fn advance_capture(&mut self, generation: u64, id: OutputId) {
        let Some(capture) = self.freeze.output_mut(generation, id) else {
            return;
        };
        let result = match capture.step {
            Step::Live => {
                let output = &self.wayland.outputs[&id];
                match output.wgpu.as_ref().unwrap().hide_annotations() {
                    Ok(Some(blank)) => {
                        output.surface.frame(&self.qhandle, (generation, id));
                        blank.present();
                        capture.step = Step::Blanking;
                        self.draw.damage(id);
                        Ok(())
                    }
                    Ok(None) => {
                        // Leave an output that cannot present now live instead of failing the others.
                        log::warn!("output {id} left live: could not hide annotations for capture");
                        let Phase::Capturing { frames, .. } = &mut self.freeze.phase else {
                            unreachable!()
                        };
                        frames.remove(&id);
                        if frames.is_empty() {
                            Err("could not hide annotations for capture".into())
                        } else {
                            self.handle_freeze(Instant::now());
                            Ok(())
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            Step::Idle => capture.allocate(&self.wayland, &self.qhandle, (generation, id)),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.fail_freeze(&format!("Screen freeze failed: {error}"));
        }
    }

    fn finish_capture(&mut self, id: OutputId, buffer: CaptureBuffer, transform: Transform) {
        let (wayland, qh) = (&mut self.wayland, &self.qhandle);
        let output = wayland.outputs.get_mut(&id).unwrap();
        let frozen = output.frozen_surface.get_or_insert_with(|| {
            let surface = wayland.compositor.create_surface(qh, id);
            let subcompositor = wayland.subcompositor.as_ref().unwrap();
            let subsurface = subcompositor.get_subsurface(&surface, &output.surface, qh, ());
            subsurface.place_below(&output.surface);
            let viewporter = wayland.viewporter.as_ref().unwrap();
            let viewport = viewporter.get_viewport(&surface, qh, ());
            let empty = wayland.compositor.create_region(qh, ());
            surface.set_input_region(Some(&empty));
            empty.destroy();
            FrozenSurface {
                surface,
                subsurface,
                viewport,
                buffer: None,
            }
        });
        frozen
            .viewport
            .set_destination(output.logical_size[0] as i32, output.logical_size[1] as i32);
        frozen.surface.set_buffer_transform(transform);
        frozen.surface.attach(Some(&buffer.proxy), 0, 0);
        frozen.surface.damage(0, 0, i32::MAX, i32::MAX);
        // Present the frozen desktop together with its parent annotations.
        frozen.surface.commit();
        frozen.buffer = Some(buffer);
        self.draw.damage(id);
        self.render(id);
        self.handle_freeze(Instant::now());
    }

    pub(super) fn handle_freeze(&mut self, now: Instant) {
        let Phase::Capturing { deadline, frames } = &self.freeze.phase else {
            return;
        };
        if frames.values().all(|f| matches!(f.step, Step::Ready)) {
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
    fn allocate(
        &mut self,
        wayland: &WaylandState,
        qh: &QueueHandle<State>,
        data: (u64, OutputId),
    ) -> Result<(), String> {
        let Some(constraints) = &self.constraints else {
            return Ok(());
        };
        let [width, height] = constraints.size;
        if self.use_dmabuf
            && let Some(buffer) = constraints.device.and_then(|device| {
                allocate_dmabuf(device, width, height, &constraints.dmabuf_formats)
            })
        {
            let params = wayland.dmabuf.as_ref().unwrap().create_params(qh, data);
            for (index, plane) in buffer.planes.iter().enumerate() {
                params.add(
                    plane.fd.as_fd(),
                    index as u32,
                    plane.offset,
                    plane.stride,
                    (buffer.modifier >> 32) as u32,
                    buffer.modifier as u32,
                );
            }
            params.create(width as i32, height as i32, buffer.format, Flags::empty());
            log::debug!(
                "capturing output {}: DMA-BUF {width}x{height}, {} planes, modifier {:#x}",
                data.1,
                buffer.planes.len(),
                buffer.modifier
            );
            self.step = Step::Allocating(PendingDmabuf {
                params,
                data: buffer,
                revision: self.revision,
            });
            return Ok(());
        }
        let format = constraints
            .format
            .ok_or("no supported capture buffer format")?;
        let shm = wayland.shm.as_ref().ok_or("wl_shm unavailable")?;
        let (proxy, fd) = create_shm_buffer(shm, qh, width, height, format)?;
        log::debug!(
            "capturing output {}: shared memory {width}x{height}",
            data.1
        );
        self.attach(proxy, Backing::Shm { _fd: fd }, qh, data);
        Ok(())
    }

    fn attach(
        &mut self,
        proxy: WlBuffer,
        backing: Backing,
        qh: &QueueHandle<State>,
        data: (u64, OutputId),
    ) {
        let [width, height] = self.constraints.as_ref().unwrap().size;
        let frame = self.session.create_frame(qh, data);
        frame.attach_buffer(&proxy);
        frame.damage_buffer(0, 0, width as i32, height as i32);
        frame.capture();
        self.step = Step::Capturing(CaptureFrame {
            proxy: frame,
            buffer: Some(CaptureBuffer {
                proxy,
                _backing: backing,
            }),
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
        if matches!(capture.step, Step::Ready) {
            return;
        }
        use ext_image_copy_capture_session_v1::Event;
        let pending = &mut capture.pending;
        match event {
            Event::BufferSize { width, height } => pending.size = [width, height],
            Event::ShmFormat {
                format: WEnum::Value(format),
            } if format_score(format) > pending.format.map_or(0, format_score) => {
                pending.format = Some(format);
            }
            Event::DmabufDevice { device } => {
                pending.device = device.as_slice().try_into().ok().map(u64::from_ne_bytes);
            }
            Event::DmabufFormat { format, modifiers } => {
                if let (modifiers, []) = modifiers.as_chunks::<8>() {
                    let modifiers = modifiers.iter().copied().map(u64::from_ne_bytes);
                    pending.dmabuf_formats.push((format, modifiers.collect()));
                }
            }
            Event::Done => {
                let constraints = std::mem::take(pending);
                let [width, height] = constraints.size;
                let error = if width == 0 || height == 0 || width.max(height) > i32::MAX as u32 {
                    "invalid capture dimensions"
                } else if constraints.format.is_none()
                    && !(capture.use_dmabuf
                        && constraints.device.is_some()
                        && !constraints.dmabuf_formats.is_empty())
                {
                    "no supported capture buffer format"
                } else {
                    capture.constraints = Some(constraints);
                    capture.revision += 1;
                    return state.advance_capture(generation, id);
                };
                state.fail_freeze(&format!("Screen freeze failed: {error}"));
            }
            Event::Stopped => {
                state.fail_freeze("Screen freeze failed: capture session stopped by compositor");
            }
            _ => {}
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
        if let Some(capture) = state.freeze.output_mut(generation, id)
            && matches!(capture.step, Step::Blanking)
        {
            capture.step = Step::Idle;
            state.advance_capture(generation, id);
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
        let Step::Capturing(frame) = &mut capture.step else {
            return;
        };
        if frame.proxy != *proxy {
            return;
        }
        use ext_image_copy_capture_frame_v1::Event;
        match event {
            Event::Transform {
                transform: WEnum::Value(transform),
            } => frame.transform = transform,
            Event::Ready => {
                let (buffer, transform) = (frame.buffer.take().unwrap(), frame.transform);
                capture.step = Step::Ready;
                state.finish_capture(id, buffer, transform);
            }
            Event::Failed {
                reason: WEnum::Value(FailureReason::BufferConstraints),
            } => {
                capture.step = Step::Idle;
                if capture.failed_revision != capture.revision {
                    // Reallocate using the latest complete batch. A failed buffer
                    // does not require the compositor to send new constraints.
                    capture.failed_revision = capture.revision;
                } else if !std::mem::take(&mut capture.use_dmabuf) {
                    // Shared memory was rejected too, so wait for new constraints.
                    return;
                }
                state.advance_capture(generation, id);
            }
            Event::Failed { reason } => state.fail_freeze(&format!(
                "Screen freeze failed: frame capture error ({reason:?})"
            )),
            _ => {}
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
        use zwp_linux_buffer_params_v1::Event;
        proxy.destroy();
        let current = state.freeze.output_mut(generation, id).and_then(|capture| {
            match std::mem::replace(&mut capture.step, Step::Idle) {
                Step::Allocating(pending) if pending.params == *proxy => Some((pending, capture)),
                step => {
                    capture.step = step;
                    None
                }
            }
        });
        let Some((PendingDmabuf { data, revision, .. }, capture)) = current else {
            if let Event::Created { buffer } = event {
                buffer.destroy();
            }
            return;
        };
        match event {
            Event::Created { buffer } if revision == capture.revision => {
                return capture.attach(
                    buffer,
                    Backing::Dmabuf { _buffer: data },
                    &state.qhandle,
                    (generation, id),
                );
            }
            Event::Created { buffer } => buffer.destroy(),
            Event::Failed => capture.use_dmabuf = false,
            _ => {}
        }
        state.advance_capture(generation, id);
    }
    wayland_client::event_created_child!(State, ZwpLinuxBufferParamsV1, [0 => (WlBuffer, ())]);
}

// The compositor samples the captured buffer beneath the transparent annotation
// surface. Vellum never maps it or imports it into its Vulkan device.
pub(super) struct FrozenSurface {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    buffer: Option<CaptureBuffer>,
}

impl Drop for FrozenSurface {
    fn drop(&mut self) {
        self.subsurface.destroy();
        self.viewport.destroy();
        self.surface.destroy();
    }
}

wayland_client::delegate_noop!(State: ignore WlBuffer);
wayland_client::delegate_noop!(State: ignore WlSubsurface);
