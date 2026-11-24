mod geometry;
mod gpu;
pub(crate) use gpu::GpuContext;
mod picker;
mod text;

use crate::text::TextSpec;
use geometry::{DrawCommand, Paint};
pub(crate) use geometry::{Geometry, LocalGeometry};
pub(crate) use text::contrasting_color;

use kurbo::Affine;
use picker::PickerState;
use std::borrow::Cow;
use text::TextState;
use vello_common::{TextureId, geometry::RectU16};

// The picker owns texture 0 and color glyph bitmaps follow these.
const ANNOTATION_CACHE_TEXTURE: TextureId = TextureId(1);

pub(crate) struct Viewport {
    pub origin: [f32; 2],
    pub scale: [f64; 2],
    pub background: Option<crate::Rgba>,
    pub shadows: bool,
    pub shadow_overlap: bool,
    pub interactive: bool,
    // Identifies the committed items so unchanged frames reuse their rendering.
    pub revision: Option<u64>,
}

pub(crate) enum SceneItem<'a> {
    Geometry(Cow<'a, Geometry>, [f32; 2], f32),
    Text(TextSpec<'a>, f32),
    Feedback(TextSpec<'a>),
}

pub(crate) struct Previews<'a> {
    pub annotations: &'a [Geometry],
    pub decorations: &'a [Geometry],
    pub cursor: Option<Geometry>,
}

pub(crate) struct WgpuState {
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    output_size: [u32; 2],
    can_shrink: bool,
    device: wgpu::Device,
    queue: wgpu::Queue,
    main_renderer: vello_hybrid::Renderer,
    main_resources: vello_hybrid::Resources,
    main_scene: vello_hybrid::Scene,
    // Built on first use to avoid compiling a second pipeline per output.
    picker: Option<PickerState>,
    annotation_cache: Option<(AnnotationKey, wgpu::TextureView)>,
    text: TextState,
    // Textures the main scene samples: the picker target, annotation cache and color glyph bitmaps.
    texture_bindings: vello_hybrid::TextureBindings,
}

// Revision, target size, origin and scale bits, and shadow overlap.
type AnnotationKey = (u64, [u16; 2], [u32; 2], [u64; 2], bool);

impl WgpuState {
    pub(crate) fn size(&self) -> [u32; 2] {
        self.output_size
    }

    pub(crate) fn new(
        gpu: &GpuContext,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        can_shrink: bool,
    ) -> Result<Self, String> {
        checked_target_size(&gpu.device, [width, height], "annotation")?;
        let output_size = [width, height];
        let [width, height] = if can_shrink { [1, 1] } else { output_size };
        let capabilities = surface.get_capabilities(&gpu.adapter);
        use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb};
        // Wayland blends encoded colors. Avoid sRGB targets (which encode RGB after
        // Vello premultiplies edge coverage) and 2-bit alpha formats like Rgb10a2Unorm.
        let format = capabilities
            .formats
            .iter()
            .copied()
            .min_by_key(|format| match format {
                Bgra8Unorm | Rgba8Unorm => 0,
                Bgra8UnormSrgb | Rgba8UnormSrgb => 1,
                _ => 2,
            })
            .ok_or("GPU adapter does not support the Wayland surface")?;
        // Vello writes premultiplied alpha. Wayland composites inherited alpha as premultiplied.
        let alpha_mode = [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ]
        .into_iter()
        .find(|mode| capabilities.alpha_modes.contains(mode))
        .ok_or("GPU surface does not support transparency")?;
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            // Wayland frame callbacks pace redraws, so Mailbox only avoids blocking on a busy output.
            present_mode: if capabilities
                .present_modes
                .contains(&wgpu::PresentMode::Mailbox)
            {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::Fifo
            },
            desired_maximum_frame_latency: 1,
            alpha_mode,
            view_formats: vec![],
        };
        log::debug!("surface configuration: {surface_config:?}");
        surface.configure(&device, &surface_config);

        let target_config = vello_hybrid::RenderTargetConfig {
            format,
            width: 1,
            height: 1,
        };
        let mut main_settings = vello_hybrid::RenderSettings::default();
        // The glyph cache is the main scene's only atlas user; 1024px avoids a 64 MiB first-use allocation.
        main_settings.memory_settings.image_atlas_config.atlas_size = (1024, 1024);
        // A grouped shadow can span the entire output, including after a resize.
        let texture_limit = device
            .limits()
            .max_texture_dimension_2d
            .min(u16::MAX.into()) as u16;
        main_settings.memory_settings.layers_config.max_texture_size =
            vello_hybrid::SizeU16::new(texture_limit);
        let (main_renderer, main_resources) =
            vello_hybrid::Renderer::new_with(&device, &target_config, main_settings);

        Ok(Self {
            surface,
            surface_config,
            output_size,
            can_shrink,
            device,
            queue,
            main_renderer,
            main_resources,
            main_scene: vello_hybrid::Scene::new(1, 1),
            picker: None,
            annotation_cache: None,
            text: TextState::default(),
            texture_bindings: vello_hybrid::TextureBindings::new(),
        })
    }

    pub(crate) fn hide_annotations(&self) -> Result<Option<wgpu::SurfaceTexture>, String> {
        let Some(frame) = self.acquire_frame()? else {
            return Ok(None);
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let view = frame.texture.create_view(&Default::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("hide annotations for capture"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        self.queue.submit(Some(encoder.finish()));
        Ok(Some(frame))
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 || [width, height] == self.output_size {
            return Ok(());
        }
        checked_target_size(&self.device, [width, height], "annotation")?;
        self.output_size = [width, height];
        Ok(())
    }

    fn resize_surface(&mut self, [width, height]: [u32; 2]) {
        if width == self.surface_config.width && height == self.surface_config.height {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        log::debug!("resizing surface to {width}x{height}");
        self.surface.configure(&self.device, &self.surface_config);
    }

    fn acquire_frame(&self) -> Result<Option<wgpu::SurfaceTexture>, String> {
        log::trace!("acquiring surface frame");
        let mut status = self.surface.get_current_texture();
        if matches!(
            status,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
        ) {
            log::debug!("reconfiguring surface after acquisition: {status:?}");
            self.surface.configure(&self.device, &self.surface_config);
            status = self.surface.get_current_texture();
        }
        let output = match status {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            status @ (wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost) => {
                log::trace!("surface acquisition deferred: {status:?}");
                return Ok(None);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("surface acquisition validation error".into());
            }
        };
        log::trace!("surface frame acquired");
        Ok(Some(output))
    }

    pub(crate) fn render(
        &mut self,
        items: &[SceneItem<'_>],
        previews: Previews<'_>,
        picker: Option<&LocalGeometry<'_>>,
        viewport: Viewport,
        before_present: impl FnOnce(),
    ) -> Result<bool, String> {
        let Viewport {
            origin: viewport_origin,
            scale,
            background,
            shadows,
            shadow_overlap,
            interactive,
            revision,
        } = viewport;
        let empty = items.is_empty()
            && previews.annotations.is_empty()
            && previews.decorations.is_empty()
            && previews.cursor.is_none()
            && picker.is_none()
            && background.is_none();
        // Keep pipelines warm while releasing full-output swapchain and depth buffers. Interactive
        // outputs keep their size because reconfiguring waits for the device to go idle.
        self.resize_surface(if empty && !interactive && self.can_shrink {
            [1, 1]
        } else {
            self.output_size
        });
        // Creation and resize validate these dimensions.
        let size = [
            self.surface_config.width as u16,
            self.surface_config.height as u16,
        ];
        let annotated = items
            .iter()
            .any(|item| !matches!(item, SceneItem::Feedback(_)));
        if (!interactive || !annotated || !shadows) && self.annotation_cache.take().is_some() {
            self.texture_bindings.remove(ANNOTATION_CACHE_TEXTURE);
        }
        let cache_key = revision
            .filter(|_| interactive && annotated && shadows)
            .map(|revision| {
                let (origin, scale) = (viewport_origin.map(f32::to_bits), scale.map(f64::to_bits));
                (revision, size, origin, scale, shadow_overlap)
            });
        let Some(output) = self.acquire_frame()? else {
            return Ok(false);
        };

        let scene_transform = Affine::scale_non_uniform(scale[0], scale[1])
            * Affine::translate((
                -f64::from(viewport_origin[0]),
                -f64::from(viewport_origin[1]),
            ));
        let cached = self.annotation_cache.as_ref().map(|(key, _)| *key);
        let cache_rebuilt = cache_key.is_some() && cached != cache_key;
        if let Some(key) = cache_key.filter(|_| cache_rebuilt) {
            let view = match self.annotation_cache.take() {
                Some((old, view)) if old.1 == size => view,
                _ => {
                    let format = self.surface_config.format;
                    let view = sampled_target(&self.device, "annotation cache", format, size);
                    self.texture_bindings
                        .insert(ANNOTATION_CACHE_TEXTURE, view.clone());
                    view
                }
            };
            self.render_scene(size, &view, |this, _, shadows| {
                this.main_scene.set_transform(scene_transform);
                let overlap = shadows.then_some(shadow_overlap);
                this.append_annotations(items, &[], scene_transform, overlap);
                Ok(())
            })?;
            self.annotation_cache = Some((key, view));
        }
        if empty {
            // Reset keeps the recorder's peak allocations. Empty frames can release
            // that CPU storage while retaining the GPU device and pipelines.
            self.main_scene = vello_hybrid::Scene::new(size[0], size[1]);
        }
        let swapchain_view = output.texture.create_view(&Default::default());
        self.render_scene(size, &swapchain_view, |this, encoder, retry_shadows| {
            let shadows = shadows && retry_shadows;
            let is_srgb = this.surface_config.format.is_srgb();
            let scene = &mut this.main_scene;
            if let Some(color) = background {
                let [width, height] = size.map(f64::from);
                scene.set_paint(vello_color(color, is_srgb));
                scene.fill_rect(&kurbo::Rect::new(0.0, 0.0, width, height));
            }
            // Cached ink already carries its shadows, so previews draw their grouped shadow
            // beneath it and only per-item shadows above it.
            let (committed, overlap) = if cache_key.is_some() {
                if shadows && !shadow_overlap && !previews.annotations.is_empty() {
                    scene.set_transform(scene_transform);
                    scene.push_filter_layer(shadow_filter(2.0, true));
                    for geometry in previews.annotations {
                        replay_geometry(scene, geometry, is_srgb);
                    }
                    scene.pop_layer();
                    scene.set_transform(Affine::IDENTITY);
                }
                let quality = peniko::ImageQuality::Low;
                let cache = ANNOTATION_CACHE_TEXTURE;
                draw_texture(scene, cache, quality, size, Affine::IDENTITY);
                (&[][..], (shadows && shadow_overlap).then_some(true))
            } else {
                (items, shadows.then_some(shadow_overlap))
            };
            scene.set_transform(scene_transform);
            this.append_annotations(committed, previews.annotations, scene_transform, overlap);
            let scene = &mut this.main_scene;
            for geometry in previews.decorations {
                replay_geometry(scene, geometry, is_srgb);
            }
            if let Some(cursor) = &previews.cursor {
                if shadows {
                    let bounds = cursor.bounds().unwrap_or_default();
                    let radius =
                        (bounds.width().min(bounds.height()) as f32 * 0.2).clamp(0.25, 2.0);
                    scene.push_filter_layer(shadow_filter(radius, false));
                }
                replay_geometry(scene, cursor, is_srgb);
                if shadows {
                    scene.pop_layer();
                }
            }

            if let Some(picker) = picker {
                let picker_state = this.picker.get_or_insert_with(|| {
                    PickerState::new(&this.device, this.surface_config.format)
                });
                picker_state.prepare(
                    &this.device,
                    &mut this.main_scene,
                    &mut this.texture_bindings,
                    &this.surface_config,
                    picker,
                    &viewport,
                )?;
                picker_state.render(&this.device, &this.queue, encoder)?;
            } else {
                this.release_picker();
            }

            this.main_scene.set_transform(scene_transform);
            let (text, mut target) = this.text_target();
            for item in items {
                if let SceneItem::Feedback(spec) = item {
                    spec.append_to_scene(&mut target, text);
                }
            }
            if cache_key.is_none() || cache_rebuilt {
                text.finish_frame(&mut target);
            }
            Ok(())
        })?;
        before_present();
        log::trace!("presenting annotation frame");
        output.present();
        log::trace!("annotation frame presented");
        Ok(true)
    }

    // Builds the main scene into `view` and submits it, retrying without shadows when filter
    // padding exceeds the hardware limit.
    fn render_scene(
        &mut self,
        [width, height]: [u16; 2],
        view: &wgpu::TextureView,
        mut build: impl FnMut(&mut Self, &mut wgpu::CommandEncoder, bool) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut shadows = true;
        loop {
            self.main_scene.reset_and_resize(width, height);
            let mut encoder = self.device.create_command_encoder(&Default::default());
            build(self, &mut encoder, shadows)?;
            let size = vello_hybrid::RenderSize {
                width: width.into(),
                height: height.into(),
            };
            match self.main_renderer.render(
                &self.main_scene,
                &mut self.main_resources,
                &self.device,
                &self.queue,
                &mut encoder,
                &size,
                view,
                &self.texture_bindings,
            ) {
                Ok(()) => {
                    log::trace!("submitting annotation frame");
                    self.queue.submit(Some(encoder.finish()));
                    return Ok(());
                }
                Err(vello_hybrid::RenderError::IntermediateTexture(
                    error @ vello_hybrid::IntermediateTextureError::TooLarge { .. },
                )) if shadows => {
                    log::debug!("rendering without shadows for this frame: {error}");
                    shadows = false;
                }
                Err(error) => return Err(format!("Vello annotation render failed: {error}")),
            }
        }
    }

    fn text_target(&mut self) -> (&mut TextState, text::TextTarget<'_>) {
        let target = text::TextTarget {
            scene: &mut self.main_scene,
            resources: &mut self.main_resources,
            texture_bindings: &mut self.texture_bindings,
            device: &self.device,
            queue: &self.queue,
            is_srgb: self.surface_config.format.is_srgb(),
        };
        (&mut self.text, target)
    }

    // Appends committed items and live previews with grouped shadows, or per-item shadows when
    // `shadow_overlap` is `Some(true)`. `None` disables shadows.
    fn append_annotations(
        &mut self,
        items: &[SceneItem<'_>],
        previews: &[Geometry],
        scene_transform: Affine,
        shadow_overlap: Option<bool>,
    ) {
        let (text, mut target) = self.text_target();
        if shadow_overlap == Some(false) {
            target.scene.push_filter_layer(shadow_filter(2.0, false));
        }
        let mut append = |item: &SceneItem<'_>| {
            let (SceneItem::Geometry(_, _, opacity) | SceneItem::Text(_, opacity)) = item else {
                return;
            };
            let layer = annotation_layer(target.scene, *opacity, shadow_overlap == Some(true));
            match item {
                SceneItem::Geometry(geometry, [0.0, 0.0], _) => {
                    replay_geometry(target.scene, geometry, target.is_srgb);
                }
                SceneItem::Geometry(geometry, [x, y], _) => {
                    let offset = Affine::translate((f64::from(*x), f64::from(*y)));
                    target.scene.set_transform(scene_transform * offset);
                    replay_geometry(target.scene, geometry, target.is_srgb);
                    target.scene.set_transform(scene_transform);
                }
                SceneItem::Text(spec, _) => spec.append_to_scene(&mut target, text),
                SceneItem::Feedback(_) => {}
            }
            if layer {
                target.scene.pop_layer();
            }
        };
        items.iter().for_each(&mut append);
        for geometry in previews {
            append(&SceneItem::Geometry(Cow::Borrowed(geometry), [0.0; 2], 1.0));
        }
        if shadow_overlap == Some(false) {
            target.scene.pop_layer();
        }
    }

    pub(crate) fn release_picker(&mut self) {
        if let Some(picker) = &mut self.picker {
            picker.release(&mut self.texture_bindings);
        }
    }
}

fn annotation_layer(scene: &mut vello_hybrid::Scene, opacity: f32, shadow: bool) -> bool {
    if !shadow && opacity == 1.0 {
        return false;
    }
    let filter = shadow.then(|| shadow_filter(2.0, false));
    scene.push_layer(None, None, Some(opacity), None, filter);
    true
}

fn shadow_filter(radius: f32, only: bool) -> vello_common::filter_effects::Filter {
    use vello_common::filter_effects::{EdgeMode, Filter, FilterPrimitive};

    let color = peniko::Color::new([0.0, 0.0, 0.0, 0.5]);
    Filter::from_primitive(if only {
        FilterPrimitive::DropShadowOnly {
            dx: 0.0,
            dy: radius,
            std_deviation: radius,
            color,
            edge_mode: EdgeMode::None,
        }
    } else {
        FilterPrimitive::DropShadow {
            dx: 0.0,
            dy: radius,
            std_deviation: radius,
            color,
            edge_mode: EdgeMode::None,
        }
    })
}

fn checked_target_size(
    device: &wgpu::Device,
    size: [u32; 2],
    label: &str,
) -> Result<[u16; 2], String> {
    let limit = device
        .limits()
        .max_texture_dimension_2d
        .min(u32::from(u16::MAX));
    if size
        .iter()
        .any(|dimension| *dimension == 0 || *dimension > limit)
    {
        return Err(format!(
            "{label} target {}x{} must have dimensions in 1..={limit}",
            size[0], size[1]
        ));
    }
    Ok([size[0] as u16, size[1] as u16])
}

fn texture_descriptor(
    label: &str,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
    [width, height]: [u16; 2],
) -> wgpu::TextureDescriptor<'_> {
    wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.into(),
            height: height.into(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    }
}

// An offscreen target that the main scene samples as a texture.
fn sampled_target(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    size: [u16; 2],
) -> wgpu::TextureView {
    let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    let texture = device.create_texture(&texture_descriptor(label, format, usage, size));
    texture.create_view(&Default::default())
}

fn draw_texture(
    scene: &mut vello_hybrid::Scene,
    texture: TextureId,
    quality: peniko::ImageQuality,
    [width, height]: [u16; 2],
    transform: Affine,
) {
    let source_region = RectU16::new(0, 0, width, height);
    let rect = vello_hybrid::SampleRect {
        source_region,
        transform,
    };
    scene.draw_texture_rects(texture, quality, [rect]);
}

fn replay_geometry(scene: &mut vello_hybrid::Scene, geometry: &Geometry, is_srgb: bool) {
    for DrawCommand { path, paint, color } in &geometry.commands {
        scene.set_paint(vello_color(*color, is_srgb));
        match paint {
            Paint::Fill(rule) => {
                scene.set_fill_rule(*rule);
                scene.fill_path(path);
            }
            Paint::Stroke(stroke) => {
                scene.set_stroke(stroke.clone());
                scene.stroke_path(path);
            }
        }
    }
}

fn srgb_to_linear(rgb: [f32; 3]) -> [f32; 3] {
    <color::Srgb as color::ColorSpace>::to_linear_srgb(rgb)
}

fn vello_color([red, green, blue, alpha]: [f32; 4], is_srgb: bool) -> peniko::Color {
    let [red, green, blue] = if is_srgb {
        srgb_to_linear([red, green, blue])
    } else {
        [red, green, blue]
    };
    peniko::Color::new([red, green, blue, alpha])
}
