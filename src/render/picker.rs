use super::{LocalGeometry, Viewport, checked_target_size, replay_geometry};
use kurbo::Affine;
use vello_common::{TextureId, geometry::RectU16};

const PICKER_RENDER_SCALE: u32 = 2;
const PICKER_TEXTURE: TextureId = TextureId(0);

struct PickerTarget {
    view: wgpu::TextureView,
    size: [u16; 2],
}

impl PickerTarget {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, size: [u16; 2]) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("picker target"),
            size: wgpu::Extent3d {
                width: u32::from(size[0]),
                height: u32::from(size[1]),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            size,
        }
    }
}

pub(super) struct PickerState {
    renderer: vello_hybrid::Renderer,
    resources: vello_hybrid::Resources,
    scene: vello_hybrid::Scene,
    target: Option<PickerTarget>,
}

impl PickerState {
    pub(super) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let target_config = vello_hybrid::RenderTargetConfig {
            format,
            width: 1,
            height: 1,
        };
        let (renderer, resources) = vello_hybrid::Renderer::new(device, &target_config);
        Self {
            renderer,
            resources,
            scene: vello_hybrid::Scene::new(1, 1),
            target: None,
        }
    }

    pub(super) fn prepare(
        &mut self,
        device: &wgpu::Device,
        scene: &mut vello_hybrid::Scene,
        bindings: &mut vello_hybrid::TextureBindings,
        config: &wgpu::SurfaceConfiguration,
        picker: &LocalGeometry,
        viewport: &Viewport,
    ) -> Result<(), String> {
        let Viewport {
            origin: viewport_origin,
            scale,
            ..
        } = *viewport;
        let picker_origin = [
            (f64::from(picker.origin[0] - viewport_origin[0]) * scale[0]) as f32,
            (f64::from(picker.origin[1] - viewport_origin[1]) * scale[1]) as f32,
        ];
        let size = [
            (f64::from(picker.size[0]) * scale[0]).ceil() as u32,
            (f64::from(picker.size[1]) * scale[1]).ceil() as u32,
        ];
        let scene_size = checked_scene_size(device, size)?;
        if self
            .target
            .as_ref()
            .is_none_or(|target| target.size != scene_size)
        {
            self.target = Some(PickerTarget::new(device, config.format, scene_size));
            bindings.insert(PICKER_TEXTURE, self.target.as_ref().unwrap().view.clone());
        }
        self.scene.reset_and_resize(scene_size[0], scene_size[1]);
        self.scene.set_transform(
            Affine::scale(f64::from(PICKER_RENDER_SCALE))
                * Affine::scale_non_uniform(scale[0], scale[1]),
        );
        replay_geometry(&mut self.scene, &picker.geometry, config.format.is_srgb());
        let origin = picker_origin.map(|value| (value - 0.5).ceil());
        let extent = [0, 1].map(|axis| {
            let end = picker_origin[axis] + picker.size[axis] as f32 * scale[axis] as f32;
            (((end - 0.5).ceil() - origin[axis]) * PICKER_RENDER_SCALE as f32)
                .clamp(0.0, f32::from(scene_size[axis])) as u16
        });
        scene.set_transform(Affine::IDENTITY);
        scene.draw_texture_rects(
            PICKER_TEXTURE,
            peniko::ImageQuality::Medium,
            [vello_hybrid::SampleRect {
                source_region: RectU16::new(0, 0, extent[0], extent[1]),
                transform: Affine::translate((f64::from(origin[0]), f64::from(origin[1])))
                    * Affine::scale(1.0 / f64::from(PICKER_RENDER_SCALE)),
            }],
        );
        Ok(())
    }

    pub(super) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), String> {
        let target = self.target.as_ref().unwrap();
        self.renderer
            .render(
                &self.scene,
                &mut self.resources,
                device,
                queue,
                encoder,
                &vello_hybrid::RenderSize {
                    width: u32::from(target.size[0]),
                    height: u32::from(target.size[1]),
                },
                &target.view,
                &vello_hybrid::TextureBindings::new(),
            )
            .map_err(|error| format!("Vello picker render failed: {error}"))
    }

    pub(super) fn release_target(&mut self, bindings: &mut vello_hybrid::TextureBindings) {
        bindings.remove(PICKER_TEXTURE);
        self.target = None;
    }
}

fn checked_scene_size(device: &wgpu::Device, size: [u32; 2]) -> Result<[u16; 2], String> {
    let scaled = size.map(|dimension| dimension.checked_mul(PICKER_RENDER_SCALE));
    let [Some(width), Some(height)] = scaled else {
        return Err(format!(
            "picker target {}x{} overflows at {PICKER_RENDER_SCALE}x scale",
            size[0], size[1]
        ));
    };
    checked_target_size(device, [width, height], "picker")
}
