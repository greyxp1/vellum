use std::collections::HashMap;

use crate::text::TextSpec;
use parley::PositionedLayoutItem;
use skrifa::MetadataProvider;
use skrifa::bitmap::{BitmapData, BitmapFormat, BitmapGlyph};
use skrifa::raw::TableProvider;
use vello_common::TextureId;
use wgpu::util::DeviceExt;

use super::{checked_target_size, draw_texture, srgb_to_linear, texture_descriptor, vello_color};

const DARK_TEXT_EMBOLDENING: f64 = 0.2;
const BLACK_BACKGROUND_COLOR: f32 = 20.0 / 255.0;
const WHITE_BACKGROUND_COLOR: f32 = 235.0 / 255.0;

pub(crate) fn contrasting_color([red, green, blue, alpha]: [f32; 4]) -> [f32; 4] {
    let [red, green, blue] = srgb_to_linear([red, green, blue]);
    let luminance = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
    let contrast = |background| {
        let background_luminance = srgb_to_linear([background; 3])[0];
        (luminance.max(background_luminance) + 0.05) / (luminance.min(background_luminance) + 0.05)
    };
    let background = if contrast(BLACK_BACKGROUND_COLOR) >= contrast(WHITE_BACKGROUND_COLOR) {
        BLACK_BACKGROUND_COLOR
    } else {
        WHITE_BACKGROUND_COLOR
    };
    [background, background, background, alpha]
}

pub(super) struct TextTarget<'a> {
    pub scene: &'a mut vello_hybrid::Scene,
    pub resources: &'a mut vello_hybrid::Resources,
    pub texture_bindings: &'a mut vello_hybrid::TextureBindings,
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub is_srgb: bool,
}

type BitmapKey = (u64, u32, u32, u32, u32);

struct BitmapImage {
    texture: TextureId,
    size: [u16; 2],
    used: bool,
}

// Bitmaps use their own textures because Hybrid's upload_image panics once the atlases it
// shares with the glyph cache are full.
#[derive(Default)]
struct Bitmaps {
    images: HashMap<BitmapKey, BitmapImage>,
    next_texture: u64,
}

impl TextSpec<'_> {
    pub(super) fn append_to_scene(&self, target: &mut TextTarget<'_>, text: &mut TextState) {
        let TextState {
            bitmaps,
            outline_glyphs,
        } = text;
        let scene = &mut *target.scene;
        let layout = self.layout;
        let target_is_srgb = target.is_srgb;
        scene.set_fill_rule(peniko::Fill::NonZero);
        let automatic_background = self
            .background_roundness
            .map(|roundness| (contrasting_color(self.color), roundness));
        let emboldening = if automatic_background.is_some_and(|(color, _)| color[0] > 0.5) {
            DARK_TEXT_EMBOLDENING
        } else {
            0.0
        };
        if let Some((background_color, roundness)) = automatic_background {
            use kurbo::Shape;
            let bounds = self.bounds();
            let background = bounds
                .to_rounded_rect(bounds.height() * 0.5 * f64::from(roundness))
                .to_path(0.1);
            scene.set_paint(vello_color(background_color, target_is_srgb));
            scene.fill_path(&background);
        }
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let [scale_x, scale_y] = self.scale;
                let synthesis = run.synthesis();
                let transform = self.glyph_transform(run);
                let emboldening = emboldening
                    + if synthesis.embolden() {
                        f64::from(run.font_size()) / 48.0
                    } else {
                        0.0
                    };
                let font =
                    skrifa::FontRef::from_index(run.font().data.as_ref(), run.font().index).ok();
                let strikes = font.as_ref().map(|font| font.bitmap_strikes());
                let units_per_em = font
                    .as_ref()
                    .and_then(|font| font.head().ok())
                    .map_or(1.0, |head| f32::from(head.units_per_em()));
                let is_sbix = strikes.as_ref().and_then(|strikes| strikes.format())
                    == Some(BitmapFormat::Sbix);
                let bitmap = |id| {
                    strikes
                        .as_ref()?
                        .glyph_for_size(
                            skrifa::instance::Size::new(run.font_size()),
                            skrifa::GlyphId::new(id),
                        )
                        .filter(|bitmap| matches!(bitmap.data, BitmapData::Png(_)))
                };
                // Hybrid's direct glyph path cannot consume bitmap pixmaps, and
                // its glyph cache excludes large sizes. Upload each native strike once.
                for glyph in glyph_run.positioned_glyphs() {
                    if let Some(mut bitmap) = bitmap(glyph.id) {
                        // Match glifo's Apple Color Emoji offset, inherited from
                        // CoreText: a zero SBIX bearing gets 100 font units.
                        if bitmap.bearing_y == 0.0 && is_sbix {
                            bitmap.bearing_y = 100.0;
                        }
                        self.draw_bitmap(target, bitmaps, run, glyph, &bitmap, units_per_em);
                    } else {
                        outline_glyphs.push(glifo::Glyph {
                            id: glyph.id,
                            x: self.left + scale_x * glyph.x,
                            y: self.top + scale_y * glyph.y,
                        });
                    }
                }
                if outline_glyphs.is_empty() {
                    continue;
                }
                target
                    .scene
                    .set_paint(vello_color(self.color, target_is_srgb));
                target
                    .scene
                    .glyph_run(target.resources, run.font())
                    .font_size(run.font_size())
                    .normalized_coords(run.normalized_coords())
                    .glyph_transform(transform)
                    .hint(true)
                    .font_embolden(glifo::FontEmbolden::new(kurbo::Diagonal2::new(
                        emboldening,
                        emboldening,
                    )))
                    .fill_glyphs(outline_glyphs.iter().copied());
                outline_glyphs.clear();
            }
        }
    }

    fn draw_bitmap(
        &self,
        target: &mut TextTarget<'_>,
        bitmaps: &mut Bitmaps,
        run: &parley::Run<'_, ()>,
        glyph: parley::Glyph,
        bitmap: &BitmapGlyph<'_>,
        units_per_em: f32,
    ) {
        let BitmapData::Png(data) = bitmap.data else {
            return;
        };
        let key = (
            run.font().data.id(),
            run.font().index,
            glyph.id,
            bitmap.ppem_x.to_bits(),
            bitmap.ppem_y.to_bits(),
        );
        let image = match bitmaps.images.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let Ok(mut pixmap) = vello_hybrid::Pixmap::from_png(std::io::Cursor::new(data))
                else {
                    return;
                };
                if target.is_srgb {
                    for pixel in pixmap.data_mut().iter_mut().filter(|pixel| pixel.a > 0) {
                        let alpha = f32::from(pixel.a);
                        let rgb =
                            [pixel.r, pixel.g, pixel.b].map(|channel| f32::from(channel) / alpha);
                        [pixel.r, pixel.g, pixel.b] =
                            srgb_to_linear(rgb).map(|channel| (channel * alpha).round() as u8);
                    }
                }
                let size = [pixmap.width(), pixmap.height()];
                if checked_target_size(target.device, size.map(u32::from), "bitmap").is_err() {
                    return;
                }
                let format = wgpu::TextureFormat::Rgba8Unorm;
                let usage = wgpu::TextureUsages::TEXTURE_BINDING;
                let descriptor = texture_descriptor("bitmap glyph", format, usage, size);
                let texture = target.device.create_texture_with_data(
                    target.queue,
                    &descriptor,
                    wgpu::util::TextureDataOrder::LayerMajor,
                    pixmap.data_as_u8_slice(),
                );
                // The picker and annotation cache own the textures before these.
                bitmaps.next_texture += 1;
                let id = TextureId(super::ANNOTATION_CACHE_TEXTURE.0 + bitmaps.next_texture);
                target
                    .texture_bindings
                    .insert(id, texture.create_view(&Default::default()));
                entry.insert(BitmapImage {
                    texture: id,
                    size,
                    used: false,
                })
            }
        };
        image.used = true;
        let saved = target.scene.save_current_state();
        let transform = saved.transforms.scene_transform()
            * self.bitmap_transform(run, glyph, bitmap, image.size[1], units_per_em);
        target.scene.set_transform(transform);
        target.scene.push_opacity_layer(self.color[3]);
        let quality = peniko::ImageQuality::Medium;
        draw_texture(
            target.scene,
            image.texture,
            quality,
            image.size,
            kurbo::Affine::IDENTITY,
        );
        target.scene.pop_layer();
        target.scene.restore_state(saved);
    }
}

#[derive(Default)]
pub(super) struct TextState {
    bitmaps: Bitmaps,
    outline_glyphs: Vec<glifo::Glyph>,
}

impl TextState {
    pub(super) fn finish_frame(&mut self, target: &mut TextTarget<'_>) {
        self.bitmaps.images.retain(|_, image| {
            let used = std::mem::take(&mut image.used);
            if !used {
                target.texture_bindings.remove(image.texture);
            }
            used
        });
    }
}
