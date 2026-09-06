use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use cosmic_text::{
    Attrs, Buffer, Cursor, Edit, Editor, Family, FontSystem, Metrics, Shaping, Wrap, fontdb,
};

use super::{srgb_to_linear, vello_color};

const DARK_TEXT_EMBOLDENING: f64 = 0.2;
const LINE_HEIGHT_SCALE: f32 = 1.25;
const BLACK_BACKGROUND_COLOR: f32 = 0.07843137255;
const WHITE_BACKGROUND_COLOR: f32 = 0.9215686275;

pub fn text_line_height(font_size: f32) -> f32 {
    font_size * LINE_HEIGHT_SCALE
}

pub fn text_padding(font_size: f32) -> [f32; 2] {
    [font_size * 0.25, font_size * 0.125]
}

pub fn text_bounds(
    [left, top]: [f32; 2],
    [width, height]: [f32; 2],
    font_size: f32,
    background_roundness: Option<f32>,
    [scale_x, scale_y]: [f32; 2],
) -> [[f32; 2]; 2] {
    let [padding_x, padding_y] = if background_roundness.is_some() {
        text_padding(font_size)
    } else {
        [0.0; 2]
    };
    let end_x = left + (width + padding_x) * scale_x;
    let end_y = top + (height + padding_y) * scale_y;
    let start_x = left - padding_x * scale_x;
    let start_y = top - padding_y * scale_y;
    let mut min_x = start_x.min(end_x);
    let min_y = start_y.min(end_y);
    let mut max_x = start_x.max(end_x);
    let max_y = start_y.max(end_y);
    if let Some(roundness) = background_roundness {
        let radius = (max_y - min_y) * 0.5 * roundness;
        let missing_width = (2.0 * radius - (max_x - min_x)).max(0.0);
        if scale_x < 0.0 {
            min_x -= missing_width;
        } else {
            max_x += missing_width;
        }
    }
    [[min_x, min_y], [max_x, max_y]]
}

fn background_color([red, green, blue, alpha]: [f32; 4]) -> [f32; 4] {
    let luminance = 0.2126 * srgb_to_linear(red)
        + 0.7152 * srgb_to_linear(green)
        + 0.0722 * srgb_to_linear(blue);
    let contrast_with_black = (luminance + 0.05) / 0.05;
    let contrast_with_white = 1.05 / (luminance + 0.05);
    if contrast_with_black >= contrast_with_white {
        [
            BLACK_BACKGROUND_COLOR,
            BLACK_BACKGROUND_COLOR,
            BLACK_BACKGROUND_COLOR,
            alpha,
        ]
    } else {
        [
            WHITE_BACKGROUND_COLOR,
            WHITE_BACKGROUND_COLOR,
            WHITE_BACKGROUND_COLOR,
            alpha,
        ]
    }
}

pub struct TextSpec<'a> {
    pub key: u64,
    pub content: &'a str,
    pub left: f32,
    pub top: f32,
    pub font_size: f32,
    pub color: [f32; 4],
    pub background_roundness: Option<f32>,
    pub scale: [f32; 2],
}

struct CachedText {
    content: String,
    font_size: f32,
    layout_size: [f32; 2],
    buffer: Buffer,
}

struct PreparedText {
    key: u64,
    left: f32,
    top: f32,
    color: [f32; 4],
    background_roundness: Option<f32>,
    scale: [f32; 2],
}

pub(super) struct TextState {
    font_system: FontSystem,
    font_cache: HashMap<fontdb::ID, peniko::FontData>,
    buffers: HashMap<u64, CachedText>,
    prepared_text: Vec<PreparedText>,
    prepared: u64,
}

impl TextState {
    pub(super) fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            font_cache: HashMap::new(),
            buffers: HashMap::new(),
            prepared_text: Vec::new(),
            prepared: 0,
        }
    }

    pub(super) fn prepare(&mut self, width: u32, height: u32, specs: &[TextSpec<'_>]) {
        let mut hasher = DefaultHasher::new();
        (width, height).hash(&mut hasher);
        for spec in specs {
            spec.key.hash(&mut hasher);
            spec.content.hash(&mut hasher);
            for value in [spec.left, spec.top, spec.font_size]
                .into_iter()
                .chain(spec.background_roundness)
                .chain(spec.scale)
                .chain(spec.color)
            {
                value.to_bits().hash(&mut hasher);
            }
        }
        let prepared = hasher.finish();
        if self.prepared == prepared {
            return;
        }

        self.buffers
            .retain(|key, _| specs.iter().any(|spec| spec.key == *key));

        for spec in specs {
            let stale = self.buffers.get(&spec.key).is_none_or(|cached| {
                cached.content != spec.content || cached.font_size != spec.font_size
            });
            if stale {
                let line_height = text_line_height(spec.font_size);
                let mut buffer = Buffer::new(
                    &mut self.font_system,
                    Metrics::new(spec.font_size, line_height),
                );
                buffer.set_wrap(&mut self.font_system, Wrap::None);
                buffer.set_size(&mut self.font_system, None, Some(line_height));
                buffer.set_text(
                    &mut self.font_system,
                    spec.content,
                    &Attrs::new().family(Family::SansSerif),
                    Shaping::Advanced,
                    None,
                );
                buffer.shape_until_scroll(&mut self.font_system, false);
                let layout_width = buffer
                    .layout_runs()
                    .fold(0.0_f32, |width, run| width.max(run.line_w));
                self.buffers.insert(
                    spec.key,
                    CachedText {
                        content: spec.content.to_owned(),
                        font_size: spec.font_size,
                        layout_size: [layout_width, line_height],
                        buffer,
                    },
                );
            }
        }

        self.prepared_text.clear();
        self.prepared_text
            .extend(specs.iter().map(|spec| PreparedText {
                key: spec.key,
                left: spec.left,
                top: spec.top,
                color: spec.color,
                background_roundness: spec.background_roundness,
                scale: spec.scale,
            }));
        self.prepared = prepared;
    }

    pub(super) fn append_to_scene(
        &mut self,
        scene: &mut vello_hybrid::Scene,
        resources: &mut vello_hybrid::Resources,
        target_is_srgb: bool,
    ) {
        let Self {
            font_system,
            font_cache,
            buffers,
            prepared_text,
            ..
        } = self;

        for prepared in prepared_text {
            let Some(cached) = buffers.get(&prepared.key) else {
                continue;
            };
            let automatic_background = prepared
                .background_roundness
                .map(|roundness| (background_color(prepared.color), roundness));
            let emboldening = if automatic_background.is_some_and(|(color, _)| color[0] > 0.5) {
                DARK_TEXT_EMBOLDENING
            } else {
                0.0
            };
            if let Some((background_color, roundness)) = automatic_background {
                use kurbo::Shape;

                let [[min_x, min_y], [max_x, max_y]] = text_bounds(
                    [prepared.left, prepared.top],
                    cached.layout_size,
                    cached.font_size,
                    Some(roundness),
                    prepared.scale,
                );
                let radius = (max_y - min_y) * 0.5 * roundness;
                let background = kurbo::RoundedRect::new(
                    f64::from(min_x),
                    f64::from(min_y),
                    f64::from(max_x),
                    f64::from(max_y),
                    f64::from(radius),
                )
                .to_path(0.1);
                scene.set_paint(vello_color(background_color, target_is_srgb));
                scene.fill_path(&background);
            }
            for run in cached.buffer.layout_runs() {
                for glyphs in run.glyphs.chunk_by(|left, right| {
                    left.font_id == right.font_id
                        && left.font_size.to_bits() == right.font_size.to_bits()
                        && left.font_weight == right.font_weight
                        && left.color_opt == right.color_opt
                }) {
                    let Some(first) = glyphs.first() else {
                        continue;
                    };
                    let color = first.color_opt.map_or(prepared.color, |color| {
                        color.as_rgba().map(|channel| f32::from(channel) / 255.0)
                    });
                    let font_data = font_cache.entry(first.font_id).or_insert_with(|| {
                        font_system
                            .db()
                            .with_face_data(first.font_id, |data, index| {
                                peniko::FontData::new(data.to_vec().into(), index)
                            })
                            .expect("load shaped annotation font")
                    });
                    let left = prepared.left;
                    let [scale_x, scale_y] = prepared.scale;
                    let baseline = prepared.top + run.line_y * scale_y;
                    let transform =
                        kurbo::Affine::scale_non_uniform(f64::from(scale_x), f64::from(scale_y));
                    let glyphs = glyphs.iter().map(move |glyph| glifo::Glyph {
                        id: u32::from(glyph.glyph_id),
                        x: left + scale_x * (glyph.x + glyph.font_size * glyph.x_offset),
                        y: baseline - scale_y * glyph.font_size * glyph.y_offset,
                    });
                    scene.set_paint(vello_color(color, target_is_srgb));
                    scene
                        .glyph_run(resources, font_data)
                        .font_size(first.font_size)
                        .glyph_transform(transform)
                        .hint(true)
                        .font_embolden(glifo::FontEmbolden::new(kurbo::Diagonal2::new(
                            emboldening,
                            emboldening,
                        )))
                        .fill_glyphs(glyphs);
                }
            }
        }
    }

    pub(super) fn layout_size(&self, key: u64) -> Option<[f32; 2]> {
        self.buffers.get(&key).map(|cached| cached.layout_size)
    }

    pub(super) fn cursor_x(&mut self, key: u64, index: usize) -> Option<f32> {
        let cached = self.buffers.get_mut(&key)?;
        let mut editor = Editor::new(&mut cached.buffer);
        editor.set_cursor(Cursor::new(0, index));
        editor.cursor_position().map(|(x, _)| x as f32)
    }
}
