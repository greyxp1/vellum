use peniko::Fill;

use super::{Point, PreeditHint, ToolCursor, scene};
use crate::render::Geometry;
use crate::text::text_line_height;
use crate::tool::Primitive;

pub(super) fn tool_cursor_geometry(point: Point, cursor: ToolCursor) -> Geometry {
    use kurbo::Shape;

    let radius = f64::from(match cursor.tool {
        Primitive::Pen | Primitive::Eraser => cursor.size * 0.5,
        _ => unreachable!("only pen and eraser have tool cursors"),
    });
    let point = if cursor.tool == Primitive::Pen {
        scene::pixel_aligned_point(point, cursor.size)
    } else {
        point
    };
    let center = kurbo::Point::new(f64::from(point.x), f64::from(point.y));
    if cursor.tool == Primitive::Eraser {
        const OUTLINE_WIDTH: f64 = 0.75;
        let mut geometry = Geometry::fill(
            kurbo::Circle::new(center, radius + OUTLINE_WIDTH).to_path(0.1),
            Fill::NonZero,
            [0.0, 0.0, 0.0, 1.0],
        );
        geometry.push_fill(
            kurbo::Circle::new(center, radius).to_path(0.1),
            Fill::NonZero,
            [1.0, 1.0, 1.0, 1.0],
        );
        return geometry;
    }

    let mut color = cursor.color;
    color[3] = color[3].sqrt();
    let corner_radius = radius * f64::from(cursor.roundness.clamp(0.0, 1.0));
    Geometry::fill(
        kurbo::RoundedRect::new(
            center.x - radius,
            center.y - radius,
            center.x + radius,
            center.y + radius,
            corner_radius,
        )
        .to_path(0.1),
        Fill::NonZero,
        color,
    )
}

pub(super) fn text_caret(left: f32, top: f32, scaled_font_size: f32) -> Geometry {
    use kurbo::Shape;

    let line_height = text_line_height(scaled_font_size);
    let caret_height = (scaled_font_size.abs() - 2.0)
        .max(1.0)
        .copysign(scaled_font_size);
    let inset = (line_height - caret_height) * 0.5;
    let top = top + inset;
    let bottom = top + caret_height;
    let (top, bottom) = (top.min(bottom), top.max(bottom));
    let mut geometry = Geometry::default();
    for (half_width, color) in [(1.0, [0.0, 0.0, 0.0, 1.0]), (0.5, [1.0, 1.0, 1.0, 1.0])] {
        geometry.push_fill(
            kurbo::Rect::new(
                f64::from(left - half_width),
                f64::from(top),
                f64::from(left + half_width),
                f64::from(bottom),
            )
            .to_path(0.1),
            Fill::NonZero,
            color,
        );
    }
    geometry
}

pub(super) fn text_cursor_rectangle(
    text_origin: Point,
    area: parley::BoundingBox,
    [scale_x, scale_y]: [f32; 2],
    output_origin: Point,
) -> [i32; 4] {
    // Text-input rectangles use logical surface coordinates, before buffer scaling.
    let x0 = text_origin.x + area.x0 as f32 * scale_x - output_origin.x;
    let y0 = text_origin.y + area.y0 as f32 * scale_y - output_origin.y;
    let x1 = text_origin.x + area.x1 as f32 * scale_x - output_origin.x;
    let y1 = text_origin.y + area.y1 as f32 * scale_y - output_origin.y;
    let left = x0.min(x1).floor();
    let top = y0.min(y1).floor();
    [
        left as i32,
        top as i32,
        (x0.max(x1).ceil() - left).max(1.0) as i32,
        (y0.max(y1).ceil() - top).max(1.0) as i32,
    ]
}

pub(super) fn text_preedit_span(
    start: f32,
    end: f32,
    top: f32,
    line_height: f32,
    style: PreeditHint,
) -> Geometry {
    use kurbo::Shape;

    let left = start.min(end);
    let right = start.max(end).max(left + 1.0);
    let bottom = top + line_height;
    let (top, bottom, color) = if style == PreeditHint::Selection {
        (top.min(bottom), top.max(bottom), [0.2, 0.45, 1.0, 0.25])
    } else {
        let color = match style {
            PreeditHint::SpellingError => [1.0, 0.15, 0.1, 1.0],
            PreeditHint::ComposeError => [1.0, 0.45, 0.05, 1.0],
            PreeditHint::Prediction => [0.55, 0.55, 0.55, 0.8],
            _ => [0.2, 0.45, 1.0, 1.0],
        };
        let baseline = bottom - 1.5;
        (baseline, baseline + 1.5, color)
    };
    Geometry::fill(
        kurbo::Rect::new(
            f64::from(left),
            f64::from(top),
            f64::from(right),
            f64::from(bottom),
        )
        .to_path(0.1),
        Fill::NonZero,
        color,
    )
}
