//! Builds annotation paths and their layout bounds.

use peniko::Fill;
use std::borrow::Cow;

use super::{Bounds, ElementKind, Point, Style, kurbo_point};
use crate::draw::{CIRCLE_KAPPA, freehand};
use crate::render::Geometry;
use crate::text::layout_text;

fn pixel_aligned_coordinate(value: f32, width: f32) -> f32 {
    if !value.is_finite() || !width.is_finite() || width <= 0.0 {
        return value;
    }
    let radius = width * 0.5;
    (value - radius + 0.5).floor() + radius
}

pub(in crate::draw) fn pixel_aligned_point(point: Point, width: f32) -> Point {
    Point::new(
        pixel_aligned_coordinate(point.x, width),
        pixel_aligned_coordinate(point.y, width),
    )
}

pub(in crate::draw) fn pixel_aligned_points(points: &[Point], width: f32) -> Cow<'_, [Point]> {
    let Some(&first) = points.first() else {
        return Cow::Borrowed(points);
    };
    let offset = pixel_aligned_point(first, width) - first;
    if offset == Point::default() {
        Cow::Borrowed(points)
    } else {
        Cow::Owned(points.iter().map(|point| *point + offset).collect())
    }
}

pub(super) fn pixel_aligned_segment(
    mut start: Point,
    mut end: Point,
    width: f32,
) -> (Point, Point) {
    let delta = end - start;
    let length = delta.length();
    if length <= f32::EPSILON {
        let point = pixel_aligned_point(start, width);
        return (point, point);
    }
    let tolerance = length * 1e-6;
    if delta.y.abs() <= tolerance {
        let y = pixel_aligned_coordinate(start.y, width);
        start.y = y;
        end.y = y;
    } else if delta.x.abs() <= tolerance {
        let x = pixel_aligned_coordinate(start.x, width);
        start.x = x;
        end.x = x;
    }
    (start, end)
}

fn pixel_aligned_rectangle(min: Point, max: Point, width: f32) -> (Point, Point) {
    (
        pixel_aligned_point(min, width),
        pixel_aligned_point(max, width),
    )
}

pub(in crate::draw) fn bounds_for(kind: &ElementKind, style: Style) -> Bounds {
    let width = style.size;
    let bounds = match kind {
        ElementKind::Segment {
            points: [start, end],
            arrow,
        } => {
            let (start, end) = pixel_aligned_segment(*start, *end, width);
            if *arrow {
                Bounds::from_points(
                    [start, end]
                        .into_iter()
                        .chain(arrow_head(start, end, width).vertices),
                )
            } else {
                Bounds::from_points([start, end])
            }
        }
        ElementKind::Freehand { points } => {
            Bounds::from_points(pixel_aligned_points(points, width).iter().copied())
        }
        ElementKind::Triangle { vertices } => {
            return crate::draw::triangle::bounds(vertices, style);
        }
        ElementKind::Rectangle { min, max } => {
            let (min, max) = pixel_aligned_rectangle(*min, *max, width);
            Bounds { min, max }
        }
        ElementKind::Ellipse { center, radii } => Bounds {
            min: Point::new(center.x - radii.x, center.y - radii.y),
            max: Point::new(center.x + radii.x, center.y + radii.y),
        },
        ElementKind::Text {
            origin,
            content,
            scale,
        } => {
            let layout = layout_text(content, style.size);
            text_bounds(
                *origin,
                [layout.full_width(), layout.height()],
                style,
                *scale,
            )
        }
    };
    let radius = width * 0.5;
    let expansion = match kind {
        ElementKind::Text { .. } => 0.0,
        ElementKind::Freehand { .. } => {
            let roundness = style.roundness.clamp(0.0, 1.0);
            radius * (std::f32::consts::SQRT_2 - (std::f32::consts::SQRT_2 - 1.0) * roundness)
        }
        _ => radius,
    };
    bounds.expanded(expansion)
}

pub(in crate::draw) fn text_bounds(
    origin: Point,
    [width, height]: [f32; 2],
    style: Style,
    scale: [f32; 2],
) -> Bounds {
    let [[min_x, min_y], [max_x, max_y]] = crate::text::text_bounds(
        [origin.x, origin.y],
        [width, height],
        style.size,
        style.filled.then_some(style.roundness),
        scale,
    );
    Bounds {
        min: Point::new(min_x, min_y),
        max: Point::new(max_x, max_y),
    }
}

pub(in crate::draw) fn geometry(kind: &ElementKind, style: Style) -> Geometry {
    use kurbo::Shape;

    match kind {
        ElementKind::Segment {
            points: [start, end],
            arrow: true,
        } => {
            let (start, end) = pixel_aligned_segment(*start, *end, style.size);
            Geometry::fill(
                arrow_path(arrow_head(start, end, style.size), style.roundness),
                Fill::NonZero,
                style.color,
            )
        }
        ElementKind::Freehand { points } => freehand::geometry(points, style),
        ElementKind::Segment {
            points: [start, end],
            arrow: false,
        } => Geometry::fill(
            line_path(*start, *end, style.size, style.roundness),
            Fill::NonZero,
            style.color,
        ),
        ElementKind::Triangle { vertices } => crate::draw::triangle::geometry(vertices, style),
        ElementKind::Rectangle { min, max } => {
            let (min, max) = pixel_aligned_rectangle(*min, *max, style.size);
            rectangle_geometry(min, max, style)
        }
        ElementKind::Ellipse { center, radii } if style.filled => {
            let half = style.size * 0.5;
            let path = kurbo::Ellipse::new(
                (f64::from(center.x), f64::from(center.y)),
                (
                    f64::from((radii.x + half).max(0.0)),
                    f64::from((radii.y + half).max(0.0)),
                ),
                0.0,
            )
            .to_path(0.1);
            Geometry::fill(path, Fill::NonZero, style.color)
        }
        ElementKind::Ellipse { center, radii } => {
            let path = kurbo::Ellipse::new(
                (f64::from(center.x), f64::from(center.y)),
                (f64::from(radii.x), f64::from(radii.y)),
                0.0,
            )
            .to_path(0.1);
            Geometry::stroke(
                path,
                kurbo::Stroke::new(f64::from(style.size))
                    .with_join(kurbo::Join::Miter)
                    .with_caps(kurbo::Cap::Butt)
                    .with_miter_limit(4.0),
                style.color,
            )
        }
        ElementKind::Text { .. } => Geometry::default(),
    }
}

pub(in crate::draw) fn rendered_segment_endpoints(
    kind: &ElementKind,
    style: Style,
) -> Option<[Point; 2]> {
    let ElementKind::Segment {
        points: [first, last],
        arrow,
    } = kind
    else {
        return None;
    };
    let (start, end) = pixel_aligned_segment(*first, *last, style.size);
    let end = if *arrow {
        rounded_tip(
            &arrow_head(start, end, style.size).vertices,
            0,
            style.roundness,
        )
    } else {
        end
    };
    Some([start, end])
}

fn rectangle_geometry(min: Point, max: Point, style: Style) -> Geometry {
    use kurbo::Shape;

    let half = style.size * 0.5;
    let maximum = (max.x - min.x).abs().min((max.y - min.y).abs()) * 0.5 + half;
    let outer_radius = (100.0 * style.roundness.clamp(0.0, 1.0)).min(maximum);
    let inner_radius = (outer_radius - style.size).max(0.0);
    let contours = [
        (
            Point::new(min.x - half, min.y - half),
            Point::new(max.x + half, max.y + half),
            outer_radius,
        ),
        (
            Point::new(min.x + half, min.y + half),
            Point::new(max.x - half, max.y - half),
            inner_radius,
        ),
    ];
    let mut path = kurbo::BezPath::new();
    let contour_count = if style.filled { 1 } else { contours.len() };
    for (min, max, radius) in contours.into_iter().take(contour_count) {
        if min.x >= max.x || min.y >= max.y {
            continue;
        }
        if radius <= f32::EPSILON {
            path.extend(
                kurbo::Rect::new(
                    f64::from(min.x),
                    f64::from(min.y),
                    f64::from(max.x),
                    f64::from(max.y),
                )
                .path_elements(0.1),
            );
        } else {
            path.extend(
                kurbo::RoundedRect::new(
                    f64::from(min.x),
                    f64::from(min.y),
                    f64::from(max.x),
                    f64::from(max.y),
                    f64::from(radius),
                )
                .path_elements(0.1),
            );
        }
    }
    Geometry::fill(path, Fill::EvenOdd, style.color)
}

#[derive(Clone, Copy)]
struct ArrowHead {
    tail: Point,
    vertices: [Point; 3],
    base: Point,
    normal: Point,
    radius: f32,
    shaft_length: f32,
}

fn arrow_head(start: Point, end: Point, width: f32) -> ArrowHead {
    let delta = end - start;
    let length = delta.length();
    if length <= f32::EPSILON {
        return ArrowHead {
            tail: start,
            vertices: [end; 3],
            base: end,
            normal: Point::default(),
            radius: width * 0.5,
            shaft_length: 0.0,
        };
    }
    let direction = Point::new(delta.x / length, delta.y / length);
    let normal = Point::new(-direction.y, direction.x);
    let ideal_size = (width * 5.0).max(16.0);
    let size = ideal_size.min(length);
    let base = Point::new(end.x - direction.x * size, end.y - direction.y * size);
    let half = size * 0.45;
    ArrowHead {
        tail: start,
        vertices: [
            end,
            Point::new(base.x + normal.x * half, base.y + normal.y * half),
            Point::new(base.x - normal.x * half, base.y - normal.y * half),
        ],
        base,
        normal,
        radius: width * 0.5,
        shaft_length: length - size,
    }
}

fn arrow_path(head: ArrowHead, roundness: f32) -> kurbo::BezPath {
    if head.shaft_length <= f32::EPSILON {
        return rounded_polygon_path(&head.vertices, roundness);
    }
    let shaft_offset = head.normal * head.radius;
    rounded_polygon_path(
        &[
            head.tail + shaft_offset,
            head.base + shaft_offset,
            head.vertices[1],
            head.vertices[0],
            head.vertices[2],
            head.base - shaft_offset,
            head.tail - shaft_offset,
        ],
        roundness,
    )
}

fn line_path(start: Point, end: Point, width: f32, roundness: f32) -> kurbo::BezPath {
    let (start, end) = pixel_aligned_segment(start, end, width);
    let delta = end - start;
    let delta_x = f64::from(delta.x);
    let delta_y = f64::from(delta.y);
    let length = delta_x.hypot(delta_y);
    let radius = f64::from(width.max(0.0)) * 0.5;
    let mut path = kurbo::BezPath::new();
    if length <= f64::EPSILON || radius <= f64::EPSILON {
        return path;
    }

    let cap = (radius * f64::from(roundness.clamp(0.0, 1.0))).min(length * 0.5);
    let control_x = cap * CIRCLE_KAPPA;
    let control_y = radius * CIRCLE_KAPPA;
    let end_center = length - cap;

    path.move_to((cap, -radius));
    path.line_to((end_center, -radius));
    path.curve_to(
        (end_center + control_x, -radius),
        (length, -control_y),
        (length, 0.0),
    );
    path.curve_to(
        (length, control_y),
        (end_center + control_x, radius),
        (end_center, radius),
    );
    path.line_to((cap, radius));
    path.curve_to((cap - control_x, radius), (0.0, control_y), (0.0, 0.0));
    path.curve_to(
        (0.0, -control_y),
        (cap - control_x, -radius),
        (cap, -radius),
    );
    path.close_path();
    let transform = if delta.y == 0.0 {
        let direction = f64::from(delta.x.signum());
        kurbo::Affine::new([
            direction,
            0.0,
            0.0,
            direction,
            f64::from(start.x),
            f64::from(start.y),
        ])
    } else if delta.x == 0.0 {
        let direction = f64::from(delta.y.signum());
        kurbo::Affine::new([
            0.0,
            direction,
            -direction,
            0.0,
            f64::from(start.x),
            f64::from(start.y),
        ])
    } else {
        kurbo::Affine::rotate(delta_y.atan2(delta_x))
            .then_translate(kurbo::Vec2::new(f64::from(start.x), f64::from(start.y)))
    };
    path.apply_affine(transform);
    path
}

fn rounded_polygon_path(vertices: &[Point], roundness: f32) -> kurbo::BezPath {
    let mut path = kurbo::BezPath::new();
    let corners = vertices.iter().enumerate().map(|(index, vertex)| {
        let (before, after) = rounded_polygon_corner(vertices, index, roundness);
        [before, *vertex, after].map(kurbo_point)
    });
    append_rounded_contour(&mut path, corners);
    path
}

pub(in crate::draw) fn append_rounded_contour(
    path: &mut kurbo::BezPath,
    corners: impl Iterator<Item = [kurbo::Point; 3]>,
) {
    for (index, [before, vertex, after]) in corners.enumerate() {
        if index == 0 {
            path.move_to(before);
        } else {
            path.line_to(before);
        }
        if before != vertex || after != vertex {
            path.quad_to(vertex, after);
        }
    }
    path.close_path();
}

fn rounded_polygon_corner(vertices: &[Point], index: usize, roundness: f32) -> (Point, Point) {
    let (before, after) = rounded_corner(
        kurbo_point(vertices[(index + vertices.len() - 1) % vertices.len()]),
        kurbo_point(vertices[index]),
        kurbo_point(vertices[(index + 1) % vertices.len()]),
        f64::from(roundness),
    );
    (
        Point::new(before.x as f32, before.y as f32),
        Point::new(after.x as f32, after.y as f32),
    )
}

/// Midpoint of a rounded corner's curve, where it passes closest to the vertex.
pub(in crate::draw) fn rounded_tip(vertices: &[Point], index: usize, roundness: f32) -> Point {
    let (before, after) = rounded_polygon_corner(vertices, index, roundness);
    (before + vertices[index] * 2.0 + after) * 0.25
}

pub(in crate::draw) fn rounded_corner(
    previous: kurbo::Point,
    vertex: kurbo::Point,
    next: kurbo::Point,
    roundness: f64,
) -> (kurbo::Point, kurbo::Point) {
    let roundness = roundness.clamp(0.0, 1.0);
    if roundness <= f64::EPSILON {
        return (vertex, vertex);
    }
    let to_previous = previous - vertex;
    let to_next = next - vertex;
    let previous_length = to_previous.hypot();
    let next_length = to_next.hypot();
    if previous_length <= f64::EPSILON || next_length <= f64::EPSILON {
        return (vertex, vertex);
    }
    let cut = 0.3 * roundness * previous_length.min(next_length);
    (
        vertex + to_previous * (cut / previous_length),
        vertex + to_next * (cut / next_length),
    )
}
