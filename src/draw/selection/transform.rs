use super::Handle;
use crate::draw::Modifiers;
use crate::draw::scene::{Bounds, ElementKind, Point, Style};

const SNAP_STEP: f32 = std::f32::consts::FRAC_PI_4;

pub(in crate::draw) fn resize(
    original: &ElementKind,
    handle: Handle,
    delta: Point,
    roundness: f32,
    modifiers: Modifiers,
    equal_side_anchor: &mut Option<usize>,
) -> ElementKind {
    if let (Some((min, max)), Some(direction)) = (original.corners(), resize_direction(handle)) {
        let (min, max) = resized_box(min, max, direction, delta, modifiers);
        return ElementKind::boxed(matches!(original, ElementKind::Ellipse { .. }), min, max);
    }
    match (original, handle) {
        (ElementKind::Triangle { vertices }, Handle::Vertex(index)) => {
            let mut vertices = *vertices;
            let target = crate::draw::triangle::dragged_vertex(&vertices, index, delta, roundness);
            vertices[index] =
                constrained_triangle_vertex(vertices, index, target, modifiers, equal_side_anchor);
            ElementKind::Triangle { vertices }
        }
        (ElementKind::Segment { points, arrow }, handle @ (Handle::Start | Handle::End)) => {
            let mut points = *points;
            let index = usize::from(handle == Handle::End);
            points[index] =
                constrained_endpoint(points[1 - index], points[index] + delta, modifiers.shift);
            ElementKind::Segment {
                points,
                arrow: *arrow,
            }
        }
        _ => original.clone(),
    }
}

/// Returns None when the drag leaves the text unchanged, avoiding float rounding on a click.
pub(in crate::draw) fn resize_text(
    original: &ElementKind,
    original_style: Style,
    original_bounds: Bounds,
    handle: Handle,
    delta: Point,
    modifiers: Modifiers,
    font_size_range: [f32; 2],
) -> Option<(ElementKind, Style, Bounds)> {
    let ElementKind::Text { text, scale, .. } = original else {
        return None;
    };
    let direction = resize_direction(handle)?;
    let original_size = Point::new(
        (original_bounds.max.x - original_bounds.min.x).max(f32::EPSILON),
        (original_bounds.max.y - original_bounds.min.y).max(f32::EPSILON),
    );
    let multiplier = if modifiers.alt { 2.0 } else { 1.0 };
    let dragged_size = Point::new(
        original_size.x + delta.x * direction.x * multiplier,
        original_size.y + delta.y * direction.y * multiplier,
    );
    if dragged_size == original_size && !modifiers.shift {
        return None;
    }
    let (scale, mut size, factor) = if modifiers.shift {
        let intrinsic = Point::new(
            original_size.x / scale[0].abs().max(f32::EPSILON),
            original_size.y / scale[1].abs().max(f32::EPSILON),
        );
        let uniform_start = if scale[0].abs() == scale[1].abs() {
            scale[0].abs()
        } else {
            1.0
        };
        let start = intrinsic * uniform_start;
        let factor = Point::new(
            1.0 + (dragged_size.x - original_size.x) / start.x,
            1.0 + (dragged_size.y - original_size.y) / start.y,
        );
        let requested_factor = match (direction.x != 0.0, direction.y != 0.0) {
            (true, true) if factor.x.abs() >= factor.y.abs() => factor.x,
            (_, true) => factor.y,
            (true, false) => factor.x,
            (false, false) => unreachable!(),
        };
        let uniform = nonzero_scale(uniform_start * requested_factor.abs(), 1.0);
        (
            [
                scale[0].signum() * factor.x.signum() * uniform,
                scale[1].signum() * factor.y.signum() * uniform,
            ],
            intrinsic * uniform,
            factor,
        )
    } else {
        let factor = Point::new(
            dragged_size.x / original_size.x,
            dragged_size.y / original_size.y,
        );
        let resized_scale = [
            nonzero_scale(scale[0] * factor.x, scale[0]),
            nonzero_scale(scale[1] * factor.y, scale[1]),
        ];
        let size = Point::new(
            original_size.x * (resized_scale[0] / scale[0]).abs(),
            original_size.y * (resized_scale[1] / scale[1]).abs(),
        );
        (resized_scale, size, factor)
    };
    let (style, scale) = bake_vertical_scale(
        original_style,
        scale,
        &mut size,
        modifiers.shift,
        font_size_range,
    );
    let bounds = proportional_text_bounds(original_bounds, direction, size, factor, modifiers.alt);
    let padding = if style.filled {
        crate::text::text_padding(style.size)
    } else {
        [0.0, 0.0]
    };
    let origin = text_origin(bounds, scale, padding);
    let mut text = text.clone();
    text.set_size(style.size);
    Some((
        ElementKind::Text {
            origin,
            text,
            scale,
        },
        style,
        bounds,
    ))
}

fn resize_direction(handle: Handle) -> Option<Point> {
    let Handle::Outline(x, y) = handle else {
        return None;
    };
    Some(Point::new(f32::from(x), f32::from(y)))
}

fn bake_vertical_scale(
    mut style: Style,
    scale: [f32; 2],
    bounds_size: &mut Point,
    uniform: bool,
    [min, max]: [f32; 2],
) -> (Style, [f32; 2]) {
    let original_font_size = style.size;
    let requested_font_size = original_font_size * scale[1].abs();
    let font_size = requested_font_size.clamp(min, max);
    let clamp_ratio = font_size / requested_font_size;
    style.size = font_size;

    if uniform {
        *bounds_size = *bounds_size * clamp_ratio;
        (style, [scale[0].signum(), scale[1].signum()])
    } else {
        bounds_size.y *= clamp_ratio;
        (
            style,
            [original_font_size * scale[0] / font_size, scale[1].signum()],
        )
    }
}

fn nonzero_scale(value: f32, sign: f32) -> f32 {
    let sign = if value.abs() < f32::EPSILON {
        sign
    } else {
        value
    };
    value.abs().max(f32::EPSILON).copysign(sign)
}

fn text_origin(bounds: Bounds, scale: [f32; 2], [padding_x, padding_y]: [f32; 2]) -> Point {
    let start = |min, max, scale: f32, padding| {
        (if scale.is_sign_negative() { max } else { min }) + padding * scale
    };
    Point::new(
        start(bounds.min.x, bounds.max.x, scale[0], padding_x),
        start(bounds.min.y, bounds.max.y, scale[1], padding_y),
    )
}

fn proportional_text_bounds(
    original: Bounds,
    direction: Point,
    size: Point,
    factor: Point,
    from_center: bool,
) -> Bounds {
    let resize_axis = |min, max, direction: f32, size, factor: f32| {
        if from_center || direction == 0.0 {
            let center = (min + max) * 0.5;
            return (center - size * 0.5, center + size * 0.5);
        }
        let anchor = if direction < 0.0 { max } else { min };
        ordered(anchor, anchor + direction * factor.signum() * size)
    };
    let (min_x, max_x) = resize_axis(
        original.min.x,
        original.max.x,
        direction.x,
        size.x,
        factor.x,
    );
    let (min_y, max_y) = resize_axis(
        original.min.y,
        original.max.y,
        direction.y,
        size.y,
        factor.y,
    );
    Bounds {
        min: Point::new(min_x, min_y),
        max: Point::new(max_x, max_y),
    }
}

fn resized_box(
    min: Point,
    max: Point,
    direction: Point,
    delta: Point,
    modifiers: Modifiers,
) -> (Point, Point) {
    let center = min.midpoint(max);
    // Returns the dragged side and the side opposite to it.
    let sides = |direction: f32, min, max| {
        if direction < 0.0 {
            (min, max)
        } else {
            (max, min)
        }
    };
    if direction.x != 0.0 && direction.y != 0.0 {
        let (x, opposite_x) = sides(direction.x, min.x, max.x);
        let (y, opposite_y) = sides(direction.y, min.y, max.y);
        let anchor = if modifiers.alt {
            center
        } else {
            Point::new(opposite_x, opposite_y)
        };
        let point = Point::new(x, y) + delta;
        return constrained_box(anchor, point, modifiers.shift, modifiers.alt);
    }
    let span = |direction: f32, min: f32, max: f32, center: f32, delta: f32| {
        if direction == 0.0 {
            return (min, max);
        }
        let (side, opposite) = sides(direction, min, max);
        let side = side + delta;
        if modifiers.alt {
            let radius = (side - center).abs();
            (center - radius, center + radius)
        } else {
            ordered(side, opposite)
        }
    };
    let (mut min_x, mut max_x) = span(direction.x, min.x, max.x, center.x, delta.x);
    let (mut min_y, mut max_y) = span(direction.y, min.y, max.y, center.y, delta.y);
    // Shift makes the unchanged axis match the resized one around its center.
    if modifiers.shift && direction.x == 0.0 {
        let half = (max_y - min_y) * 0.5;
        (min_x, max_x) = (center.x - half, center.x + half);
    } else if modifiers.shift {
        let half = (max_x - min_x) * 0.5;
        (min_y, max_y) = (center.y - half, center.y + half);
    }
    (Point::new(min_x, min_y), Point::new(max_x, max_y))
}

fn ordered(first: f32, second: f32) -> (f32, f32) {
    (first.min(second), first.max(second))
}

pub(in crate::draw) fn constrained_endpoint(start: Point, end: Point, snap: bool) -> Point {
    if !snap {
        return end;
    }
    let delta = end - start;
    let distance = delta.length();
    let angle = (delta.y.atan2(delta.x) / SNAP_STEP).round() * SNAP_STEP;
    Point::new(
        start.x + distance * angle.cos(),
        start.y + distance * angle.sin(),
    )
}

fn constrained_triangle_vertex(
    vertices: [Point; 3],
    index: usize,
    target: Point,
    modifiers: Modifiers,
    equal_side_anchor: &mut Option<usize>,
) -> Point {
    if !modifiers.shift && !modifiers.alt {
        return target;
    }
    let first = vertices[(index + 1) % 3];
    let second = vertices[(index + 2) % 3];
    let edge = second - first;
    let edge_length = edge.length();
    if edge_length <= f32::EPSILON {
        return target;
    }
    let midpoint = first.midpoint(second);
    let normal = Point::new(-edge.y / edge_length, edge.x / edge_length);

    let project_to_line = |origin: Point, direction: Point| {
        let offset = target - origin;
        origin + direction * (offset.x * direction.x + offset.y * direction.y)
    };
    match (modifiers.shift, modifiers.alt) {
        (true, false) => nearest_point(
            target,
            [first, second, midpoint].map(|origin| project_to_line(origin, normal)),
        ),
        (false, true) => {
            let center_index =
                latched_equal_side(vertices, index, target, edge_length, equal_side_anchor);
            let center = vertices[center_index];
            let offset = target - center;
            let distance = offset.length();
            if distance <= f32::EPSILON {
                target
            } else {
                center + offset * (edge_length / distance)
            }
        }
        (true, true) => {
            let equilateral_height = edge_length * 3.0_f32.sqrt() * 0.5;
            nearest_point(
                target,
                [
                    midpoint + normal * equilateral_height,
                    midpoint - normal * equilateral_height,
                    first + normal * edge_length,
                    first - normal * edge_length,
                    second + normal * edge_length,
                    second - normal * edge_length,
                ],
            )
        }
        (false, false) => unreachable!(),
    }
}

fn nearest_point<const N: usize>(target: Point, candidates: [Point; N]) -> Point {
    candidates
        .into_iter()
        .min_by(|first, second| {
            first
                .distance_squared(target)
                .total_cmp(&second.distance_squared(target))
        })
        .unwrap_or(target)
}

fn latched_equal_side(
    vertices: [Point; 3],
    index: usize,
    target: Point,
    edge_length: f32,
    equal_side_anchor: &mut Option<usize>,
) -> usize {
    *equal_side_anchor.get_or_insert_with(|| {
        let first_index = (index + 1) % 3;
        let second_index = (index + 2) % 3;
        let first_distance = ((target - vertices[first_index]).length() - edge_length).abs();
        let second_distance = ((target - vertices[second_index]).length() - edge_length).abs();
        if first_distance <= second_distance {
            first_index
        } else {
            second_index
        }
    })
}

pub(in crate::draw) fn triangle_from_drag(
    start: Point,
    current: Point,
    modifiers: Modifiers,
) -> [Point; 3] {
    if modifiers.alt {
        let vertex = constrained_endpoint(start, current, modifiers.shift);
        let radius = vertex - start;
        let (sin, cos) = (std::f32::consts::TAU / 3.0).sin_cos();
        let rotate = |sin: f32| {
            Point::new(
                start.x + radius.x * cos - radius.y * sin,
                start.y + radius.x * sin + radius.y * cos,
            )
        };
        return [vertex, rotate(sin), rotate(-sin)];
    }
    let base = constrained_endpoint(start, current, modifiers.shift);
    let axis = base - start;
    let height = axis.length();
    if height <= f32::EPSILON {
        return [start; 3];
    }
    let half_base = height / 3.0_f32.sqrt();
    let normal = Point::new(-axis.y / height, axis.x / height) * half_base;
    [start, base + normal, base - normal]
}

pub(in crate::draw) fn constrained_box(
    start: Point,
    end: Point,
    square: bool,
    from_center: bool,
) -> (Point, Point) {
    let mut delta = end - start;
    if square {
        let size = delta.x.abs().max(delta.y.abs());
        delta.x = delta.x.signum() * size;
        delta.y = delta.y.signum() * size;
    }
    let end = start + delta;
    if from_center {
        (
            Point::new(start.x - delta.x.abs(), start.y - delta.y.abs()),
            Point::new(start.x + delta.x.abs(), start.y + delta.y.abs()),
        )
    } else {
        (
            Point::new(start.x.min(end.x), start.y.min(end.y)),
            Point::new(start.x.max(end.x), start.y.max(end.y)),
        )
    }
}
