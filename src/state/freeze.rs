use std::collections::BTreeMap;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use memmap2::MmapMut;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_output::Transform;
use wayland_client::protocol::wl_shm;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{
    self, ExtImageCopyCaptureFrameV1, FailureReason,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::Options;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{
    self, ExtImageCopyCaptureSessionV1,
};

use super::{DrawOn, OutputId, State};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct Freeze {
    pub on_activate: bool,
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

enum OutputState {
    Negotiating {
        size: Option<(u32, u32)>,
        format: Option<wl_shm::Format>,
    },
    Blanking {
        size: (u32, u32),
        format: wl_shm::Format,
    },
    Capturing {
        frame: ExtImageCopyCaptureFrameV1,
        buffer: ShmBuffer,
        transform: Transform,
    },
    Ready,
}

struct OutputCapture {
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    state: OutputState,
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        if let OutputState::Capturing { frame, .. } = &self.state {
            frame.destroy();
        }
        self.session.destroy();
        self.source.destroy();
    }
}

struct ShmBuffer {
    buffer: WlBuffer,
    mmap: MmapMut,
    width: u32,
    height: u32,
    is_bgra: bool,
    has_alpha_channel: bool,
}

impl ShmBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        width: u32,
        height: u32,
        pixel_format: wl_shm::Format,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!(
                "invalid output dimensions: width={width},height={height}"
            ));
        };
        let width_i32 =
            i32::try_from(width).map_err(|_| format!("output width {width} exceeds i32::MAX"))?;
        let height_i32 = i32::try_from(height)
            .map_err(|_| format!("output height {height} exceeds i32::MAX"))?;

        let stride = width
            .checked_mul(4)
            .ok_or_else(|| format!("stride arithmetic overflowed for width {width}"))?;
        let stride_i32 =
            i32::try_from(stride).map_err(|_| format!("stride {stride} exceeds i32::MAX"))?;

        let size = (stride)
            .checked_mul(height)
            .ok_or_else(|| format!("buffer size overflowed for {width}x{height}"))?;
        let size_i32 =
            i32::try_from(size).map_err(|_| format!("buffer size {size} exceeds i32::MAX"))?;

        let flags = rustix::fs::MemfdFlags::ALLOW_SEALING | rustix::fs::MemfdFlags::CLOEXEC;
        let fd =
            rustix::io::retry_on_intr(|| rustix::fs::memfd_create(c"vellum-freeze-shm", flags))
                .map_err(|e| format!("memfd_create failed: {e}"))?;

        rustix::fs::ftruncate(&fd, u64::from(size))
            .map_err(|e| format!("ftruncate failed: {e}"))?;

        let seals = rustix::fs::SealFlags::SHRINK
            | rustix::fs::SealFlags::GROW
            | rustix::fs::SealFlags::SEAL;
        rustix::fs::fcntl_add_seals(&fd, seals)
            .map_err(|e| format!("fcntl_add_seals failed: {e}"))?;

        let mmap = unsafe { MmapMut::map_mut(&fd).map_err(|e| format!("mmap failed: {e}"))? };

        let pool = shm.create_pool(fd.as_fd(), size_i32, qh, ());
        let buffer = pool.create_buffer(0, width_i32, height_i32, stride_i32, pixel_format, qh, ());
        pool.destroy();

        let is_bgra = matches!(
            pixel_format,
            wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888
        );
        let has_alpha_channel = matches!(
            pixel_format,
            wl_shm::Format::Argb8888 | wl_shm::Format::Abgr8888
        );

        Ok(Self {
            buffer,
            mmap,
            width,
            height,
            is_bgra,
            has_alpha_channel,
        })
    }

    fn ensure_opaque(&mut self) {
        for pixel in self.mmap.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl Freeze {
    pub fn new(on_activate: bool) -> Self {
        Self {
            on_activate,
            generation: 0,
            phase: Phase::Live,
        }
    }

    pub fn capturing(&self) -> bool {
        matches!(self.phase, Phase::Capturing { .. })
    }

    pub fn hides(&self, output: OutputId) -> bool {
        matches!(
            &self.phase,
            Phase::Capturing { frames, .. } if frames.get(&output).is_some_and(|f| {
                matches!(
                    f.state,
                    OutputState::Blanking { .. } | OutputState::Capturing { .. }
                )
            })
        )
    }

    pub fn next_wakeup(&self) -> Option<Instant> {
        match &self.phase {
            Phase::Capturing { deadline, .. } => Some(*deadline),
            _ => None,
        }
    }

    fn capturing_frame_mut(&mut self, generation: u64, id: OutputId) -> Option<&mut OutputCapture> {
        if generation != self.generation {
            return None;
        }
        match &mut self.phase {
            Phase::Capturing { frames, .. } => frames.get_mut(&id),
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
        if self.draw_on == DrawOn::Current && self.selected_output.is_none() {
            self.freeze.phase = Phase::WaitingOutput;
            return;
        }

        let (Some(_shm), Some(image_copy), Some(image_source)) = (
            &self.wayland.shm,
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
            if self.draw_on == DrawOn::Current && self.selected_output != Some(id) {
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

        let frames = outputs
            .into_iter()
            .map(|id| {
                let output = &self.wayland.outputs[&id];
                let source = image_source.create_source(&output.output, &self.qhandle, ());
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
                        state: OutputState::Negotiating {
                            size: None,
                            format: None,
                        },
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
        }
    }

    pub(super) fn stop_freeze(&mut self, deactivating: bool) {
        let old_phase = std::mem::replace(&mut self.freeze.phase, Phase::Live);
        let mut damaged = Vec::new();
        for (&id, output) in &mut self.wayland.outputs {
            let Some(wgpu) = &mut output.wgpu else {
                continue;
            };
            let was_capturing =
                matches!(&old_phase, Phase::Capturing { frames, .. } if frames.contains_key(&id));
            if wgpu.is_frozen() || was_capturing {
                wgpu.clear_frozen_background();
                self.draw.damage(id);
                damaged.push(id);
            }
        }
        if !deactivating {
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

    fn blank_output(
        &mut self,
        output_id: OutputId,
        size: (u32, u32),
        format: wl_shm::Format,
    ) -> Result<(), String> {
        let output = &self.wayland.outputs[&output_id];
        let blank = output
            .wgpu
            .as_ref()
            .unwrap()
            .hide_annotations()?
            .ok_or("could not hide annotations for capture")?;

        output
            .surface
            .frame(&self.qhandle, (self.freeze.generation, output_id));
        blank.present();

        if let Some(capturing) = self
            .freeze
            .capturing_frame_mut(self.freeze.generation, output_id)
        {
            capturing.state = OutputState::Blanking { size, format };
        }
        self.draw.damage(output_id);
        Ok(())
    }

    fn capture_output(&mut self, generation: u64, output_id: OutputId) -> Result<(), String> {
        let Some(capturing) = self.freeze.capturing_frame_mut(generation, output_id) else {
            return Ok(());
        };
        let OutputState::Blanking {
            size: (width, height),
            format,
        } = capturing.state
        else {
            return Ok(());
        };

        let shm = self.wayland.shm.as_ref().ok_or("wl_shm unavailable")?;
        let buffer = ShmBuffer::new(shm, &self.qhandle, width, height, format)?;

        let frame = capturing
            .session
            .create_frame(&self.qhandle, (generation, output_id));
        frame.attach_buffer(&buffer.buffer);
        frame.damage_buffer(0, 0, buffer.width as i32, buffer.height as i32);
        frame.capture();

        capturing.state = OutputState::Capturing {
            frame,
            buffer,
            transform: Transform::Normal,
        };
        Ok(())
    }

    pub(super) fn handle_freeze(&mut self, now: Instant) {
        let Phase::Capturing { deadline, .. } = &self.freeze.phase else {
            return;
        };
        if now >= *deadline {
            self.fail_freeze("Screen freeze timed out; returned to live drawing");
        }
    }
}

fn format_score(fmt: wl_shm::Format) -> usize {
    match fmt {
        wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => 2,
        wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => 1,
        _ => 0,
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, (u64, OutputId)> for State {
    fn event(
        state: &mut Self,
        session_proxy: &ExtImageCopyCaptureSessionV1,
        event: <ExtImageCopyCaptureSessionV1 as Proxy>::Event,
        &(generation, output_id): &(u64, OutputId),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        match event {
            Event::BufferSize { width, height } => {
                if let Some(capturing) = state.freeze.capturing_frame_mut(generation, output_id)
                    && let OutputState::Negotiating { size, .. } = &mut capturing.state
                {
                    *size = Some((width, height));
                }
            }
            Event::ShmFormat {
                format: WEnum::Value(f),
            } => {
                if let Some(capturing) = state.freeze.capturing_frame_mut(generation, output_id)
                    && let OutputState::Negotiating { format, .. } = &mut capturing.state
                    && format_score(f) > format.map_or(0, format_score)
                {
                    *format = Some(f);
                }
            }
            Event::Done => {
                let Some(capturing) = state.freeze.capturing_frame_mut(generation, output_id)
                else {
                    return;
                };

                let OutputState::Negotiating {
                    size: Some(size),
                    format: Some(format),
                } = capturing.state
                else {
                    state.fail_freeze("Screen freeze failed: invalid size or format");
                    return;
                };

                if let Err(error) = state.blank_output(output_id, size, format) {
                    state.fail_freeze(&format!("Screen freeze failed: {error}"));
                }
            }
            Event::Stopped => {
                session_proxy.destroy();
                if generation == state.freeze.generation {
                    state
                        .fail_freeze("Screen freeze failed: capture session stopped by compositor");
                }
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
        &(generation, output): &(u64, OutputId),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Err(error) = state.capture_output(generation, output) {
            state.fail_freeze(&format!("Screen freeze failed: {error}"));
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, (u64, OutputId)> for State {
    fn event(
        state: &mut Self,
        frame_proxy: &ExtImageCopyCaptureFrameV1,
        event: <ExtImageCopyCaptureFrameV1 as Proxy>::Event,
        &(generation, output): &(u64, OutputId),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        match event {
            Event::Transform {
                transform: WEnum::Value(t),
            } => {
                if let Some(capturing) = state.freeze.capturing_frame_mut(generation, output)
                    && let OutputState::Capturing { transform, .. } = &mut capturing.state
                {
                    *transform = t;
                }
            }
            Event::Ready => {
                frame_proxy.destroy();

                let Some(capturing) = state.freeze.capturing_frame_mut(generation, output) else {
                    return;
                };
                let OutputState::Capturing {
                    mut buffer,
                    transform,
                    ..
                } = std::mem::replace(&mut capturing.state, OutputState::Ready)
                else {
                    state.fail_freeze("Screen freeze failed: frame not in capturing state");
                    return;
                };

                if !buffer.has_alpha_channel {
                    buffer.ensure_opaque();
                }

                let wgpu = state
                    .wayland
                    .outputs
                    .get_mut(&output)
                    .unwrap()
                    .wgpu
                    .as_mut()
                    .unwrap();

                let res = wgpu.set_frozen_background(
                    [buffer.width, buffer.height],
                    &buffer.mmap,
                    buffer.is_bgra,
                    transform,
                );
                drop(buffer);

                if let Err(e) = res {
                    state.fail_freeze(&format!("Screen freeze failed: {e}"));
                    return;
                }

                state.draw.damage(output);
                state.render(output);

                let all_ready = match &state.freeze.phase {
                    Phase::Capturing { frames, .. } => frames
                        .values()
                        .all(|f| matches!(f.state, OutputState::Ready)),
                    _ => false,
                };
                if all_ready {
                    state.freeze.phase = Phase::Frozen;
                    state.request_render();
                }
            }
            Event::Failed { reason } => {
                frame_proxy.destroy();
                if generation != state.freeze.generation {
                    return;
                }
                let reason = match reason {
                    WEnum::Value(r) => r,
                    WEnum::Unknown(_) => FailureReason::Unknown,
                };
                state.fail_freeze(&format!(
                    "Screen freeze failed: frame capture error ({reason:?})"
                ));
            }
            _ => {}
        }
    }
}
