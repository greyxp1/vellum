//! Builds annotation paths and their layout bounds.

use peniko::Fill;
use std::borrow::Cow;

use super::{Bounds, ElementKind, Point, Style, kurbo_point};
use crate::draw::{CIRCLE_KAPPA, freehand, triangle};
use crate::render::Geometry;
use crate::text::TextSpec;
use kurbo::Shape;
use parley::Layout;

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

pub(in crate::draw) fn bounds_for(kind: &ElementKind, style: Style) -> Bounds {
    let width = style.size;
    let bounds = match kind {
        ElementKind::Segment {
            points: [start, end],
            arrow,
        } => {
            let (start, end) = pixel_aligned_segment(*start, *end, width);
            let head = arrow.then(|| arrow_head(start, end, width).vertices);
            Bounds::from_points([start, end].into_iter().chain(head.into_iter().flatten()))
        }
        ElementKind::Freehand { points, .. } => {
            Bounds::from_points(pixel_aligned_points(points, width).iter().copied())
        }
        ElementKind::Triangle { vertices } => {
            return triangle::outline(vertices, style).bounding_box().into();
        }
        ElementKind::Rectangle { min, max } => Bounds {
            min: pixel_aligned_point(*min, width),
            max: pixel_aligned_point(*max, width),
        },
        ElementKind::Ellipse { center, radii } => Bounds {
            min: *center - *radii,
            max: *center + *radii,
        },
        ElementKind::Text {
            origin,
            text,
            scale,
        } => {
            return text_spec(text.layout(), *origin, style, *scale)
                .bounds()
                .into();
        }
    };
    let radius = width * 0.5;
    let expansion = if let ElementKind::Freehand { .. } = kind {
        let roundness = style.roundness.clamp(0.0, 1.0);
        radius * (std::f32::consts::SQRT_2 - (std::f32::consts::SQRT_2 - 1.0) * roundness)
    } else {
        radius
    };
    bounds.expanded(expansion)
}

pub(in crate::draw) fn text_spec(
    layout: &Layout<()>,
    origin: Point,
    style: Style,
    scale: [f32; 2],
) -> TextSpec<'_> {
    TextSpec {
        layout,
        left: origin.x,
        top: origin.y,
        font_size: style.size,
        color: style.color,
        background_roundness: style.filled.then_some(style.roundness),
        scale,
    }
}

/// Builds bounds and geometry together, reusing a triangle's outline for both.
pub(super) fn shape(kind: &ElementKind, style: Style) -> (Bounds, Geometry) {
    let ElementKind::Triangle { vertices } = kind else {
        return (bounds_for(kind, style), geometry(kind, style));
    };
    let outline = triangle::outline(vertices, style);
    (
        outline.bounding_box().into(),
        triangle::geometry(outline, style),
    )
}

pub(in crate::draw) fn geometry(kind: &ElementKind, style: Style) -> Geometry {
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
        ElementKind::Freehand { points, .. } => freehand::geometry(points, style),
        ElementKind::Segment {
            points: [start, end],
            arrow: false,
        } => Geometry::fill(
            line_path(*start, *end, style.size, style.roundness),
            Fill::NonZero,
            style.color,
        ),
        ElementKind::Triangle { vertices } => {
            triangle::geometry(triangle::outline(vertices, style), style)
        }
        ElementKind::Rectangle { min, max } => rectangle_geometry(
            pixel_aligned_point(*min, style.size),
            pixel_aligned_point(*max, style.size),
            style,
        ),
        ElementKind::Ellipse { center, radii } => {
            // Filled ellipses cover the outline that strokes would draw around their radii.
            let half = if style.filled { style.size * 0.5 } else { 0.0 };
            let radii = (radii.x + half, radii.y + half);
            let radii = (f64::from(radii.0.max(0.0)), f64::from(radii.1.max(0.0)));
            let path = kurbo::Ellipse::new(kurbo_point(*center), radii, 0.0).to_path(0.1);
            if style.filled {
                return Geometry::fill(path, Fill::NonZero, style.color);
            }
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
    let half = style.size * 0.5;
    let maximum = (max.x - min.x).abs().min((max.y - min.y).abs()) * 0.5 + half;
    let outer_radius = (100.0 * style.roundness.clamp(0.0, 1.0)).min(maximum);
    let inner_radius = (outer_radius - style.size).max(0.0);
    let bounds = Bounds { min, max };
    let contours = [
        (bounds.expanded(half), outer_radius),
        (bounds.expanded(-half), inner_radius),
    ];
    let mut path = kurbo::BezPath::new();
    let contour_count = if style.filled { 1 } else { contours.len() };
    for (bounds, radius) in contours.into_iter().take(contour_count) {
        if bounds.min.x >= bounds.max.x || bounds.min.y >= bounds.max.y {
            continue;
        }
        let rect = kurbo::Rect::from(bounds);
        if radius <= f32::EPSILON {
            path.extend(rect.path_elements(0.1));
        } else {
            path.extend(rect.to_rounded_rect(f64::from(radius)).path_elements(0.1));
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
    let mut path = kurbo::BezPath::new();
    let roundness = f64::from(roundness);
    if head.shaft_length <= f32::EPSILON {
        append_rounded_polygon(&mut path, &head.vertices.map(kurbo_point), roundness);
        return path;
    }
    let shaft_offset = head.normal * head.radius;
    let outline = [
        head.tail + shaft_offset,
        head.base + shaft_offset,
        head.vertices[1],
        head.vertices[0],
        head.vertices[2],
        head.base - shaft_offset,
        head.tail - shaft_offset,
    ];
    append_rounded_polygon(&mut path, &outline.map(kurbo_point), roundness);
    path
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

/// Appends a closed polygon whose corners are cut and joined by quadratic curves.
pub(in crate::draw) fn append_rounded_polygon(
    path: &mut kurbo::BezPath,
    vertices: &[kurbo::Point],
    roundness: f64,
) {
    let count = vertices.len();
    for (index, &vertex) in vertices.iter().enumerate() {
        let (before, after) = rounded_corner(
            vertices[(index + count - 1) % count],
            vertex,
            vertices[(index + 1) % count],
            roundness,
        );
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

/// Midpoint of a rounded corner's curve, where it passes closest to the vertex.
pub(in crate::draw) fn rounded_tip(vertices: &[Point], index: usize, roundness: f32) -> Point {
    let count = vertices.len();
    let (before, after) = rounded_corner(
        kurbo_point(vertices[(index + count - 1) % count]),
        kurbo_point(vertices[index]),
        kurbo_point(vertices[(index + 1) % count]),
        f64::from(roundness),
    );
    let [before, after] = [before, after].map(|point| Point::new(point.x as f32, point.y as f32));
    (before + vertices[index] * 2.0 + after) * 0.25
}

fn rounded_corner(
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
