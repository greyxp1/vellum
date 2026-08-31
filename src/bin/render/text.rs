use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use cosmic_text::{
    Attrs, Buffer, Cursor, Edit, Editor, Family, FontSystem, Metrics, Shaping, Wrap, fontdb,
};

use super::vello_color;

const LINE_HEIGHT_SCALE: f32 = 1.25;

pub fn text_line_height(font_size: f32) -> f32 {
    font_size * LINE_HEIGHT_SCALE
}

pub struct TextSpec<'a> {
    pub key: u64,
    pub content: &'a str,
    pub left: f32,
    pub top: f32,
    pub font_size: f32,
    pub color: [f32; 4],
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
                    scene.set_paint(vello_color(color, target_is_srgb));
                    let font_data = font_cache.entry(first.font_id).or_insert_with(|| {
                        font_system
                            .db()
                            .with_face_data(first.font_id, |data, index| {
                                peniko::FontData::new(data.to_vec().into(), index)
                            })
                            .expect("load shaped annotation font")
                    });
                    let left = prepared.left;
                    let baseline = prepared.top + run.line_y;
                    scene
                        .glyph_run(resources, font_data)
                        .font_size(first.font_size)
                        .hint(true)
                        .atlas_cache(true)
                        .fill_glyphs(glyphs.iter().map(move |glyph| glifo::Glyph {
                            id: u32::from(glyph.glyph_id),
                            x: left + glyph.x + glyph.font_size * glyph.x_offset,
                            y: baseline - glyph.font_size * glyph.y_offset,
                        }));
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
