mod geometry;
mod text;

pub use geometry::{DrawCommand, FillRule, Geometry, LocalGeometry, StrokeStyle};
pub use text::{TextSpec, text_bounds, text_line_height};

use kurbo::Affine;
use text::TextState;
use wayland_client::Proxy;
use wayland_client::protocol::wl_display::WlDisplay;
use wayland_client::protocol::wl_surface::WlSurface;
use wgpu::util::DeviceExt;

const PICKER_RENDER_SCALE: u32 = 2;

struct PickerTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    composite_buffer: wgpu::Buffer,
    size: [u32; 2],
    origin: [f32; 2],
}

fn composite_bytes(origin: [f32; 2]) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[..4].copy_from_slice(&origin[0].to_ne_bytes());
    bytes[4..8].copy_from_slice(&origin[1].to_ne_bytes());
    bytes
}

impl PickerTarget {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: [u32; 2],
        origin: [f32; 2],
        layout: &wgpu::BindGroupLayout,
    ) -> Option<Self> {
        let width = size[0].checked_mul(PICKER_RENDER_SCALE)?;
        let height = size[1].checked_mul(PICKER_RENDER_SCALE)?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("picker target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let composite_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("picker composite origin"),
            contents: &composite_bytes(origin),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("picker composite"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: composite_buffer.as_entire_binding(),
                },
            ],
        });
        Some(Self {
            _texture: texture,
            view,
            bind_group,
            composite_buffer,
            size,
            origin,
        })
    }
}

pub struct WgpuState {
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    main_renderer: vello_hybrid::Renderer,
    main_resources: vello_hybrid::Resources,
    picker_renderer: vello_hybrid::Renderer,
    picker_resources: vello_hybrid::Resources,
    main_scene: vello_hybrid::Scene,
    picker_scene: vello_hybrid::Scene,
    texture_bindings: vello_hybrid::TextureBindings,
    committed: Geometry,
    picker_composite_pipeline: wgpu::RenderPipeline,
    picker_composite_layout: wgpu::BindGroupLayout,
    picker_target: Option<PickerTarget>,
    text: Option<TextState>,
}

impl WgpuState {
    pub fn new(display: &WlDisplay, surface: &WlSurface, width: u32, height: u32) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let raw_display_handle =
            wgpu::rwh::RawDisplayHandle::Wayland(wgpu::rwh::WaylandDisplayHandle::new(
                std::ptr::NonNull::new(display.id().as_ptr() as *mut _).unwrap(),
            ));
        let raw_window_handle =
            wgpu::rwh::RawWindowHandle::Wayland(wgpu::rwh::WaylandWindowHandle::new(
                std::ptr::NonNull::new(surface.id().as_ptr() as *mut _).unwrap(),
            ));
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display_handle),
                raw_window_handle,
            })
        }
        .unwrap();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .unwrap();
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .find(|format| format.is_srgb())
            .copied()
            .unwrap_or(capabilities.formats[0]);
        let alpha_mode = capabilities
            .alpha_modes
            .iter()
            .find(|mode| matches!(mode, wgpu::CompositeAlphaMode::PreMultiplied))
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .unwrap();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        let picker_composite_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("picker composite"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let picker_composite_shader =
            device.create_shader_module(wgpu::include_wgsl!("picker_composite.wgsl"));
        let picker_composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("picker composite pipeline"),
                bind_group_layouts: &[Some(&picker_composite_layout)],
                immediate_size: 0,
            });
        let picker_composite_pipeline = create_picker_composite_pipeline(
            &device,
            &picker_composite_pipeline_layout,
            &picker_composite_shader,
            format,
        );
        let target_config = vello_hybrid::RenderTargetConfig {
            format,
            width: 1,
            height: 1,
        };
        let mut main_settings = vello_hybrid::RenderSettings::default();
        // Text is the main scene's only atlas user; 1024px avoids a 64 MiB first-use allocation.
        main_settings.memory_settings.image_atlas_config.atlas_size = (1024, 1024);
        let (main_renderer, main_resources) =
            vello_hybrid::Renderer::new_with(&device, &target_config, main_settings);
        let (picker_renderer, picker_resources) =
            vello_hybrid::Renderer::new(&device, &target_config);

        Self {
            surface,
            surface_config,
            device,
            queue,
            main_renderer,
            main_resources,
            picker_renderer,
            picker_resources,
            main_scene: vello_hybrid::Scene::new(1, 1),
            picker_scene: vello_hybrid::Scene::new(1, 1),
            texture_bindings: vello_hybrid::TextureBindings::new(),
            committed: Geometry::empty(),
            picker_composite_pipeline,
            picker_composite_layout,
            picker_target: None,
            text: None,
        }
    }

    pub fn set_committed_geometry<'a>(
        &mut self,
        geometries: impl IntoIterator<Item = &'a Geometry>,
    ) {
        self.committed.commands.clear();
        for geometry in geometries {
            self.committed
                .commands
                .extend(geometry.commands.iter().cloned());
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0
            || height == 0
            || (width == self.surface_config.width && height == self.surface_config.height)
        {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    pub fn prepare_text(&mut self, text_specs: &[TextSpec<'_>]) {
        if !text_specs.is_empty() && self.text.is_none() {
            self.text = Some(TextState::new());
        }
        if let Some(text) = &mut self.text {
            text.prepare(
                self.surface_config.width,
                self.surface_config.height,
                text_specs,
            );
        }
    }

    pub fn text_layout_size(&self, key: u64) -> Option<[f32; 2]> {
        self.text.as_ref()?.layout_size(key)
    }

    pub fn text_cursor_x(&mut self, key: u64, index: usize) -> Option<f32> {
        self.text.as_mut()?.cursor_x(key, index)
    }

    pub fn render(&mut self, previews: &[Geometry], picker: Option<&LocalGeometry>) -> bool {
        self.render_surface(previews, picker)
    }

    fn composite_picker(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        source: &PickerTarget,
        viewport: Option<[f32; 4]>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composite picker"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.picker_composite_pipeline);
        if let Some([x, y, width, height]) = viewport {
            pass.set_viewport(x, y, width, height, 0.0, 1.0);
        }
        pass.set_bind_group(0, &source.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn render_surface(&mut self, previews: &[Geometry], picker: Option<&LocalGeometry>) -> bool {
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(output)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
                    status => {
                        eprintln!("vellum: surface retry failed: {status:?}");
                        return false;
                    }
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return false;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("vellum: surface acquisition validation error");
                return false;
            }
        };

        let Some(main_size) = checked_scene_size(
            [self.surface_config.width, self.surface_config.height],
            "annotation",
        ) else {
            return false;
        };
        self.main_scene.reset_and_resize(main_size[0], main_size[1]);
        self.main_scene.set_transform(Affine::IDENTITY);
        let target_is_srgb = self.surface_config.format.is_srgb();
        replay_geometry(&mut self.main_scene, &self.committed, target_is_srgb);
        if let Some(text) = &mut self.text {
            text.append_to_scene(
                &mut self.main_scene,
                &mut self.main_resources,
                target_is_srgb,
            );
        }
        for geometry in previews {
            replay_geometry(&mut self.main_scene, geometry, target_is_srgb);
        }

        let picker_size = if let Some(picker) = picker {
            let Some(scene_size) = checked_picker_scene_size(picker.size) else {
                return false;
            };
            if self
                .picker_target
                .as_ref()
                .is_none_or(|target| target.size != picker.size)
            {
                self.picker_target = PickerTarget::new(
                    &self.device,
                    self.surface_config.format,
                    picker.size,
                    picker.origin,
                    &self.picker_composite_layout,
                );
                if self.picker_target.is_none() {
                    eprintln!("vellum: picker render target dimensions overflow");
                    return false;
                }
            }
            let target = self.picker_target.as_mut().unwrap();
            if target.origin != picker.origin {
                self.queue.write_buffer(
                    &target.composite_buffer,
                    0,
                    &composite_bytes(picker.origin),
                );
                target.origin = picker.origin;
            }
            Some(scene_size)
        } else {
            None
        };

        if let (Some(picker), Some(size)) = (picker, picker_size) {
            self.picker_scene.reset_and_resize(size[0], size[1]);
            self.picker_scene
                .set_transform(Affine::scale(f64::from(PICKER_RENDER_SCALE)));
            replay_geometry(&mut self.picker_scene, &picker.geometry, target_is_srgb);
        }

        let swapchain_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let render_size = vello_hybrid::RenderSize {
            width: u32::from(main_size[0]),
            height: u32::from(main_size[1]),
        };
        if let Err(error) = self.main_renderer.render(
            &self.main_scene,
            &mut self.main_resources,
            &self.device,
            &self.queue,
            &mut encoder,
            &render_size,
            &swapchain_view,
            &self.texture_bindings,
        ) {
            eprintln!("vellum: Vello annotation render failed: {error}");
            return false;
        }
        if let (Some(picker), Some(size)) = (picker, picker_size) {
            let render_size = vello_hybrid::RenderSize {
                width: u32::from(size[0]),
                height: u32::from(size[1]),
            };
            if let Err(error) = self.picker_renderer.render(
                &self.picker_scene,
                &mut self.picker_resources,
                &self.device,
                &self.queue,
                &mut encoder,
                &render_size,
                &self.picker_target.as_ref().unwrap().view,
                &self.texture_bindings,
            ) {
                eprintln!("vellum: Vello picker render failed: {error}");
                return false;
            }
            let left = picker.origin[0].max(0.0);
            let top = picker.origin[1].max(0.0);
            let right =
                (picker.origin[0] + picker.size[0] as f32).min(self.surface_config.width as f32);
            let bottom =
                (picker.origin[1] + picker.size[1] as f32).min(self.surface_config.height as f32);
            if right > left && bottom > top {
                self.composite_picker(
                    &mut encoder,
                    &swapchain_view,
                    wgpu::LoadOp::Load,
                    self.picker_target.as_ref().unwrap(),
                    Some([left, top, right - left, bottom - top]),
                );
            }
        }
        self.queue.submit(Some(encoder.finish()));
        output.present();
        true
    }

    pub fn release_picker_target(&mut self) {
        self.picker_target = None;
    }
}

fn checked_scene_size(size: [u32; 2], label: &str) -> Option<[u16; 2]> {
    let width = size[0].try_into().ok();
    let height = size[1].try_into().ok();
    match (width, height) {
        (Some(width), Some(height)) => Some([width, height]),
        _ => {
            eprintln!(
                "vellum: {label} target {}x{} exceeds Vello Hybrid's u16 scene dimensions",
                size[0], size[1]
            );
            None
        }
    }
}

fn checked_picker_scene_size(size: [u32; 2]) -> Option<[u16; 2]> {
    let width = size[0]
        .checked_mul(PICKER_RENDER_SCALE)
        .and_then(|value| value.try_into().ok());
    let height = size[1]
        .checked_mul(PICKER_RENDER_SCALE)
        .and_then(|value| value.try_into().ok());
    match (width, height) {
        (Some(width), Some(height)) => Some([width, height]),
        _ => {
            eprintln!(
                "vellum: picker target {}x{} at {}x exceeds Vello Hybrid's u16 scene dimensions",
                size[0], size[1], PICKER_RENDER_SCALE
            );
            None
        }
    }
}

fn replay_geometry(scene: &mut vello_hybrid::Scene, geometry: &Geometry, target_is_srgb: bool) {
    if geometry.is_empty() {
        return;
    }
    for command in &geometry.commands {
        match command {
            DrawCommand::Fill {
                path,
                fill_rule,
                color,
            } => {
                scene.set_paint(vello_color(*color, target_is_srgb));
                scene.set_fill_rule(match fill_rule {
                    FillRule::NonZero => peniko::Fill::NonZero,
                    FillRule::EvenOdd => peniko::Fill::EvenOdd,
                });
                scene.fill_path(path);
            }
            DrawCommand::Stroke {
                path,
                stroke,
                color,
            } => {
                scene.set_paint(vello_color(*color, target_is_srgb));
                scene.set_stroke(stroke.as_kurbo());
                scene.stroke_path(path);
            }
        }
    }
}

fn srgb_to_linear(component: f32) -> f32 {
    if component <= 0.04045 {
        component / 12.92
    } else {
        ((component + 0.055) / 1.055).powf(2.4)
    }
}

fn vello_color([red, green, blue, alpha]: [f32; 4], target_is_srgb: bool) -> peniko::Color {
    let convert = |component| {
        if target_is_srgb {
            srgb_to_linear(component)
        } else {
            component
        }
    };
    peniko::Color::new([convert(red), convert(green), convert(blue), alpha])
}

fn create_picker_composite_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("picker composite pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[("render_scale", PICKER_RENDER_SCALE as f64)],
                ..Default::default()
            },
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}
