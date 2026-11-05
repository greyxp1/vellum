//! Font discovery, shaping and metrics shared by editing and rendering.

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::Arc;

use parley::{Alignment, FontContext, Layout, LayoutContext, LineHeight, StyleProperty};
use skrifa::bitmap::{BitmapGlyph, Origin};

const LINE_HEIGHT_SCALE: f32 = 1.25;

pub(crate) struct TextSpec<'a> {
    pub layout: &'a Layout<()>,
    pub left: f32,
    pub top: f32,
    pub font_size: f32,
    pub color: [f32; 4],
    pub background_roundness: Option<f32>,
    pub scale: [f32; 2],
}

impl TextSpec<'_> {
    pub(crate) fn glyph_transform(&self, run: &parley::Run<'_, ()>) -> kurbo::Affine {
        let skew = f64::from(run.synthesis().skew().unwrap_or(0.0))
            .to_radians()
            .tan();
        kurbo::Affine::scale_non_uniform(f64::from(self.scale[0]), f64::from(self.scale[1]))
            * kurbo::Affine::new([1.0, 0.0, -skew, 1.0, 0.0, 0.0])
    }

    pub(crate) fn bitmap_transform(
        &self,
        run: &parley::Run<'_, ()>,
        glyph: parley::Glyph,
        bitmap: &BitmapGlyph<'_>,
        image_height: u16,
        units_per_em: f32,
    ) -> kurbo::Affine {
        let size = run.font_size();
        let origin_y = if bitmap.placement_origin == Origin::BottomLeft {
            -f64::from(image_height)
        } else {
            0.0
        };
        kurbo::Affine::translate((
            f64::from(self.left + self.scale[0] * glyph.x),
            f64::from(self.top + self.scale[1] * glyph.y),
        )) * kurbo::Affine::scale_non_uniform(f64::from(self.scale[0]), f64::from(self.scale[1]))
            * kurbo::Affine::translate((
                f64::from(-bitmap.bearing_x * size / units_per_em),
                f64::from(bitmap.bearing_y * size / units_per_em),
            ))
            * kurbo::Affine::scale_non_uniform(
                f64::from(size / bitmap.ppem_x),
                f64::from(size / bitmap.ppem_y),
            )
            * kurbo::Affine::translate((
                f64::from(-bitmap.inner_bearing_x),
                f64::from(-bitmap.inner_bearing_y) + origin_y,
            ))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextFont {
    pub family: Vec<parley::style::FontFamilyName<'static>>,
    pub weight: parley::style::FontWeight,
    pub style: parley::style::FontStyle,
    pub features: Vec<parley::style::FontFeature>,
}

pub(crate) fn text_styles(font: &TextFont) -> [StyleProperty<'static, ()>; 5] {
    [
        StyleProperty::FontFamily(parley::style::FontFamily::List(Cow::Owned(
            font.family.clone(),
        ))),
        StyleProperty::FontWeight(font.weight),
        StyleProperty::FontStyle(font.style),
        StyleProperty::FontFeatures(parley::style::FontFeatures::List(Cow::Owned(
            font.features.clone(),
        ))),
        StyleProperty::LineHeight(LineHeight::FontSizeRelative(LINE_HEIGHT_SCALE)),
    ]
}

#[derive(Clone, Debug)]
pub(crate) struct Text {
    pub content: Arc<str>,
    pub font: Arc<TextFont>,
    size: f32,
    layout: Arc<Layout<()>>,
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.content == other.content && self.font == other.font && self.size == other.size
    }
}

impl Text {
    pub(crate) fn new(content: impl Into<Arc<str>>, font: Arc<TextFont>, size: f32) -> Self {
        let content = content.into();
        let layout = layout_text(&content, size, &font);
        Self::from_layout(content, font, size, layout)
    }

    pub(crate) fn from_layout(
        content: impl Into<Arc<str>>,
        font: Arc<TextFont>,
        size: f32,
        layout: Layout<()>,
    ) -> Self {
        Self {
            content: content.into(),
            font,
            size,
            layout: Arc::new(layout),
        }
    }

    pub(crate) fn layout(&self) -> &Layout<()> {
        &self.layout
    }

    pub(crate) fn set_size(&mut self, size: f32) {
        if self.size != size {
            self.layout = Arc::new(layout_text(&self.content, size, &self.font));
            self.size = size;
        }
    }
}

thread_local! {
    // The event loop and all outputs share font discovery and shaping caches.
    static TEXT_CONTEXT: RefCell<(FontContext, LayoutContext<()>)> =
        RefCell::new((FontContext::new(), LayoutContext::new()));
}

pub(crate) fn with_text_context<T>(
    f: impl FnOnce(&mut FontContext, &mut LayoutContext<()>) -> T,
) -> T {
    TEXT_CONTEXT.with_borrow_mut(|(fonts, layouts)| f(fonts, layouts))
}

pub(crate) fn text_line_height(font_size: f32) -> f32 {
    font_size * LINE_HEIGHT_SCALE
}

pub(crate) fn text_padding(font_size: f32) -> [f32; 2] {
    [font_size * 0.25, font_size * 0.125]
}

pub(crate) fn text_bounds(
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
    // Expand in text space so the background and its editing bounds stretch together.
    let width = background_roundness.map_or(width, |roundness| {
        width.max((height + 2.0 * padding_y) * roundness - 2.0 * padding_x)
    });
    let end_x = left + (width + padding_x) * scale_x;
    let end_y = top + (height + padding_y) * scale_y;
    let start_x = left - padding_x * scale_x;
    let start_y = top - padding_y * scale_y;
    let min_x = start_x.min(end_x);
    let min_y = start_y.min(end_y);
    let max_x = start_x.max(end_x);
    let max_y = start_y.max(end_y);
    [[min_x, min_y], [max_x, max_y]]
}

fn layout_text(content: &str, font_size: f32, font: &TextFont) -> Layout<()> {
    with_text_context(|fonts, layouts| {
        let mut builder = layouts.ranged_builder(fonts, content, 1.0, false);
        for style in text_styles(font) {
            builder.push_default(style);
        }
        builder.push_default(StyleProperty::FontSize(font_size));
        let mut layout = builder.build(content);
        layout.break_all_lines(None);
        layout.align(Alignment::Start, Default::default());
        layout
    })
}
