//! Shared GPU device and the Wayland raw-handle boundary.

use std::sync::{Arc, OnceLock};

use wayland_client::Proxy;
use wayland_client::protocol::wl_display::WlDisplay;
use wayland_client::protocol::wl_surface::WlSurface;

use super::WgpuState;

pub(crate) struct GpuContext {
    instance: wgpu::Instance,
    pub(super) adapter: wgpu::Adapter,
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    lost: Arc<OnceLock<String>>,
}

impl GpuContext {
    pub(crate) fn lost(&self) -> Option<&String> {
        self.lost.get()
    }

    pub(crate) fn new(
        display: &WlDisplay,
        surface: &WlSurface,
        width: u32,
        height: u32,
    ) -> Result<(Self, WgpuState), String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::default().with_env(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = create_surface(&instance, display, surface)?;
        let adapter = match std::env::var("WGPU_ADAPTER_NAME") {
            Ok(name) => {
                let name = name.to_lowercase();
                pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
                    .into_iter()
                    .find(|adapter| {
                        adapter.get_info().name.to_lowercase().contains(&name)
                            && adapter.is_surface_supported(&surface)
                    })
                    .ok_or_else(|| {
                        format!("no Wayland-compatible Vulkan adapter matches {name:?}")
                    })?
            }
            Err(std::env::VarError::NotPresent) => {
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::default(),
                    force_fallback_adapter: false,
                    compatible_surface: Some(&surface),
                }))
                .map_err(|error| format!("could not select a Vulkan adapter: {error}"))?
            }
            Err(error) => return Err(format!("invalid WGPU_ADAPTER_NAME: {error}")),
        };
        log::info!("GPU: {:?}", adapter.get_info());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|error| format!("could not create the GPU device: {error}"))?;
        let lost = Arc::new(OnceLock::new());
        device.set_device_lost_callback({
            let lost = lost.clone();
            move |reason, message| {
                if reason != wgpu::DeviceLostReason::Destroyed {
                    let _ = lost.set(format!("GPU device lost ({reason:?}): {message}"));
                }
            }
        });
        let gpu = Self {
            instance,
            adapter,
            device,
            queue,
            lost,
        };
        let wgpu = WgpuState::new(&gpu, surface, width, height)?;
        Ok((gpu, wgpu))
    }

    pub(crate) fn create_surface(
        &self,
        display: &WlDisplay,
        surface: &WlSurface,
    ) -> Result<wgpu::Surface<'static>, String> {
        create_surface(&self.instance, display, surface)
    }
}

fn create_surface(
    instance: &wgpu::Instance,
    display: &WlDisplay,
    surface: &WlSurface,
) -> Result<wgpu::Surface<'static>, String> {
    let raw_display_handle =
        wgpu::rwh::RawDisplayHandle::Wayland(wgpu::rwh::WaylandDisplayHandle::new(
            std::ptr::NonNull::new(display.id().as_ptr() as *mut _).unwrap(),
        ));
    let raw_window_handle =
        wgpu::rwh::RawWindowHandle::Wayland(wgpu::rwh::WaylandWindowHandle::new(
            std::ptr::NonNull::new(surface.id().as_ptr() as *mut _).unwrap(),
        ));
    // SAFETY: These proxies belong to the live connection retained by State.
    // State drops each output's renderer before destroying its wl_surface, and
    // drops all renderers and the GPU before the connection (see State::drop).
    unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display_handle),
            raw_window_handle,
        })
    }
    .map_err(|error| format!("could not create the Wayland GPU surface: {error}"))
}
