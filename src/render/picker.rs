use super::{
    LocalGeometry, Viewport, checked_target_size, draw_texture, replay_geometry, sampled_target,
    vello_color,
};
use crate::config::SvgCommand;
use crate::tool::ICON_COLOR;
use kurbo::{Affine, Rect, Shape};
use vello_common::TextureId;

const PICKER_RENDER_SCALE: u32 = 2;
const PICKER_TEXTURE: TextureId = TextureId(0);

pub(super) struct PickerState {
    renderer: vello_hybrid::Renderer,
    resources: vello_hybrid::Resources,
    scene: vello_hybrid::Scene,
    target: Option<(wgpu::TextureView, [u16; 2])>,
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
        picker: &LocalGeometry<'_>,
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
            .is_none_or(|(_, size)| *size != scene_size)
        {
            let view = sampled_target(device, "picker target", config.format, scene_size);
            bindings.insert(PICKER_TEXTURE, view.clone());
            self.target = Some((view, scene_size));
        }
        let layer = &mut self.scene;
        layer.reset_and_resize(scene_size[0], scene_size[1]);
        let transform = Affine::scale(f64::from(PICKER_RENDER_SCALE))
            * Affine::scale_non_uniform(scale[0], scale[1]);
        let is_srgb = config.format.is_srgb();
        layer.set_transform(transform);
        replay_geometry(layer, &picker.geometry, is_srgb);
        for (svg, position) in &picker.svg_icons {
            let [x, y] = svg.size.map(|dimension| dimension * 0.5);
            layer.set_transform(transform * Affine::translate(*position));
            layer.set_fill_rule(peniko::Fill::NonZero);
            layer.push_clip_path(&Rect::new(-x, -y, x, y).to_path(0.02));
            for command in &svg.commands {
                match command {
                    SvgCommand::Fill {
                        path,
                        rule,
                        opacity,
                    } => {
                        let [red, green, blue, _] = ICON_COLOR;
                        layer.set_fill_rule(*rule);
                        layer.set_paint(vello_color([red, green, blue, *opacity], is_srgb));
                        layer.fill_path(path);
                    }
                    SvgCommand::PushClip { path, rule } => {
                        layer.set_fill_rule(*rule);
                        layer.push_clip_path(path);
                    }
                    SvgCommand::PopClip => layer.pop_clip_path(),
                }
            }
            layer.pop_clip_path();
        }
        let origin = picker_origin.map(|value| (value - 0.5).ceil());
        let extent = [0, 1].map(|axis| {
            let end = picker_origin[axis] + picker.size[axis] as f32 * scale[axis] as f32;
            (((end - 0.5).ceil() - origin[axis]) * PICKER_RENDER_SCALE as f32)
                .clamp(0.0, f32::from(scene_size[axis])) as u16
        });
        scene.set_transform(Affine::IDENTITY);
        let transform = Affine::translate((f64::from(origin[0]), f64::from(origin[1])))
            * Affine::scale(1.0 / f64::from(PICKER_RENDER_SCALE));
        let quality = peniko::ImageQuality::Medium;
        draw_texture(scene, PICKER_TEXTURE, quality, extent, transform);
        Ok(())
    }

    pub(super) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), String> {
        let (view, [width, height]) = self.target.as_ref().unwrap();
        let size = vello_hybrid::RenderSize {
            width: (*width).into(),
            height: (*height).into(),
        };
        let bindings = vello_hybrid::TextureBindings::new();
        let resources = &mut self.resources;
        self.renderer
            .render(
                &self.scene,
                resources,
                device,
                queue,
                encoder,
                &size,
                view,
                &bindings,
            )
            .map_err(|error| format!("Vello picker render failed: {error}"))
    }

    pub(super) fn release(&mut self, bindings: &mut vello_hybrid::TextureBindings) {
        if self.target.take().is_some() {
            bindings.remove(PICKER_TEXTURE);
            // Reset keeps the recorder's peak allocations.
            self.scene = vello_hybrid::Scene::new(1, 1);
        }
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
