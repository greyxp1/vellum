use super::Modifiers;
use super::scene::{Bounds, ElementKind, Point, Style, geometry, rendered_path_endpoints};
use crate::render::{FillRule, Geometry};

const SNAP_STEP: f32 = std::f32::consts::FRAC_PI_4;
const ENDPOINT_HIT_RADIUS: f32 = 9.0;
const OUTLINE_HIT_RADIUS: f32 = 5.0;
const VISUAL_RADIUS: f32 = 4.5;
const SELECTION_WIDTH: f32 = 1.5;
const GAP: f32 = 4.0;
const COLOR: [f32; 4] = [0.1, 0.75, 1.0, 0.8];
const HANDLE_FILL: [f32; 4] = [0.04, 0.04, 0.04, 1.0];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Handle {
    Start,
    End,
    Vertex(usize),
    Corner(Corner),
    Edge(Edge),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum CursorHint {
    #[default]
    Crosshair,
    Move,
    NsResize,
    EwResize,
    NwseResize,
    NeswResize,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

pub(super) fn cursor(handle: Handle) -> CursorHint {
    match handle {
        Handle::Corner(Corner::TopLeft | Corner::BottomRight) => CursorHint::NwseResize,
        Handle::Corner(Corner::TopRight | Corner::BottomLeft) => CursorHint::NeswResize,
        Handle::Edge(Edge::Top | Edge::Bottom) => CursorHint::NsResize,
        Handle::Edge(Edge::Left | Edge::Right) => CursorHint::EwResize,
        Handle::Start | Handle::End | Handle::Vertex(_) => CursorHint::Crosshair,
    }
}

pub(super) fn hit_handle(
    kind: &ElementKind,
    style: Style,
    bounds: Bounds,
    point: Point,
) -> Option<Handle> {
    triangle_vertex_handle(kind, style, point)
        .or_else(|| {
            rendered_path_endpoints(kind, style).and_then(|[start, end]| {
                let radius_squared = ENDPOINT_HIT_RADIUS * ENDPOINT_HIT_RADIUS;
                let start_distance = start.distance_squared(point);
                let end_distance = end.distance_squared(point);
                let (handle, distance) = if start_distance < end_distance {
                    (Handle::Start, start_distance)
                } else {
                    (Handle::End, end_distance)
                };
                (distance <= radius_squared).then_some(handle)
            })
        })
        .or_else(|| outline_handle(kind, bounds, point))
}

fn triangle_vertex_handle(kind: &ElementKind, style: Style, point: Point) -> Option<Handle> {
    let ElementKind::Triangle { vertices } = kind else {
        return None;
    };
    let radius_squared = ENDPOINT_HIT_RADIUS * ENDPOINT_HIT_RADIUS;
    super::triangle::rendered_vertices(vertices, style.roundness)
        .iter()
        .enumerate()
        .map(|(index, vertex)| (index, vertex.distance_squared(point)))
        .filter(|(_, distance)| *distance <= radius_squared)
        .min_by(|(_, first), (_, second)| first.total_cmp(second))
        .map(|(index, _)| Handle::Vertex(index))
}

pub(super) fn outline(min: Point, max: Point) -> Geometry {
    geometry(
        &ElementKind::Rectangle {
            min: Point::new(min.x - GAP, min.y - GAP),
            max: Point::new(max.x + GAP, max.y + GAP),
        },
        Style {
            size: SELECTION_WIDTH,
            color: COLOR,
            roundness: 0.0,
            filled: false,
        },
    )
}

pub(super) fn append_handles(kind: &ElementKind, style: Style, output: &mut Vec<Geometry>) {
    if let ElementKind::Triangle { vertices } = kind {
        output.extend(
            super::triangle::rendered_vertices(vertices, style.roundness)
                .into_iter()
                .map(endpoint_geometry),
        );
        return;
    }
    if let Some([start, end]) = rendered_path_endpoints(kind, style) {
        let start_geometry = endpoint_geometry(start);
        let end_geometry = start_geometry.translated([end.x - start.x, end.y - start.y]);
        output.extend([start_geometry, end_geometry]);
    }
}

fn outline_handle(kind: &ElementKind, bounds: Bounds, point: Point) -> Option<Handle> {
    if !matches!(
        kind,
        ElementKind::Rectangle { .. } | ElementKind::Ellipse { .. } | ElementKind::Text { .. }
    ) {
        return None;
    }
    let min = Point::new(bounds.min.x - GAP, bounds.min.y - GAP);
    let max = Point::new(bounds.max.x + GAP, bounds.max.y + GAP);
    if point.x < min.x - OUTLINE_HIT_RADIUS
        || point.x > max.x + OUTLINE_HIT_RADIUS
        || point.y < min.y - OUTLINE_HIT_RADIUS
        || point.y > max.y + OUTLINE_HIT_RADIUS
    {
        return None;
    }

    let left = (point.x - min.x).abs();
    let right = (point.x - max.x).abs();
    let x_edge = (left.min(right) <= OUTLINE_HIT_RADIUS).then_some(if left < right {
        Edge::Left
    } else {
        Edge::Right
    });
    let top = (point.y - min.y).abs();
    let bottom = (point.y - max.y).abs();
    let y_edge = (top.min(bottom) <= OUTLINE_HIT_RADIUS).then_some(if top < bottom {
        Edge::Top
    } else {
        Edge::Bottom
    });

    match (x_edge, y_edge) {
        (Some(Edge::Left), Some(Edge::Top)) => Some(Handle::Corner(Corner::TopLeft)),
        (Some(Edge::Right), Some(Edge::Top)) => Some(Handle::Corner(Corner::TopRight)),
        (Some(Edge::Right), Some(Edge::Bottom)) => Some(Handle::Corner(Corner::BottomRight)),
        (Some(Edge::Left), Some(Edge::Bottom)) => Some(Handle::Corner(Corner::BottomLeft)),
        (Some(edge), None) | (None, Some(edge)) => Some(Handle::Edge(edge)),
        _ => None,
    }
}

fn endpoint_geometry(center: Point) -> Geometry {
    use kurbo::Shape;

    let mut output = Geometry::fill(
        kurbo::Circle::new(
            (f64::from(center.x), f64::from(center.y)),
            f64::from(VISUAL_RADIUS),
        )
        .to_path(0.05),
        FillRule::NonZero,
        HANDLE_FILL,
    );
    let radius = VISUAL_RADIUS - SELECTION_WIDTH * 0.5;
    output.append(geometry(
        &ElementKind::Ellipse {
            center,
            radii: Point::new(radius, radius),
        },
        Style {
            size: SELECTION_WIDTH,
            color: COLOR,
            roundness: 0.0,
            filled: false,
        },
    ));
    output
}

pub(super) fn resize(
    original: &ElementKind,
    handle: Handle,
    delta: Point,
    roundness: f32,
    modifiers: Modifiers,
    equal_side_anchor: &mut Option<usize>,
) -> ElementKind {
    match (original, handle) {
        (ElementKind::Triangle { vertices }, Handle::Vertex(index)) if index < vertices.len() => {
            let mut vertices = *vertices;
            let target = super::triangle::dragged_vertex(&vertices, index, delta, roundness);
            vertices[index] =
                constrained_triangle_vertex(vertices, index, target, modifiers, equal_side_anchor);
            ElementKind::Triangle { vertices }
        }
        (
            ElementKind::Path {
                points,
                smooth: false,
                end_marker,
            },
            handle @ (Handle::Start | Handle::End),
        ) if points.len() >= 2 => {
            let mut points = points.clone();
            match handle {
                Handle::Start => {
                    points[0] = constrained_endpoint(
                        *points.last().expect("non-empty path"),
                        points[0] + delta,
                        modifiers.shift,
                    );
                }
                Handle::End => {
                    let start = points[0];
                    let end = *points.last().expect("non-empty path") + delta;
                    *points.last_mut().expect("non-empty path") =
                        constrained_endpoint(start, end, modifiers.shift);
                }
                _ => unreachable!(),
            }
            ElementKind::Path {
                points,
                smooth: false,
                end_marker: *end_marker,
            }
        }
        (ElementKind::Rectangle { min, max }, handle @ (Handle::Corner(_) | Handle::Edge(_))) => {
            let (min, max) = resized_box(*min, *max, handle, delta, modifiers);
            ElementKind::Rectangle { min, max }
        }
        (
            ElementKind::Ellipse { center, radii },
            handle @ (Handle::Corner(_) | Handle::Edge(_)),
        ) => {
            let min = Point::new(center.x - radii.x, center.y - radii.y);
            let max = Point::new(center.x + radii.x, center.y + radii.y);
            let (min, max) = resized_box(min, max, handle, delta, modifiers);
            ElementKind::Ellipse {
                center: min.midpoint(max),
                radii: Point::new((max.x - min.x) * 0.5, (max.y - min.y) * 0.5),
            }
        }
        _ => original.clone(),
    }
}

pub(super) fn resize_text(
    original: &ElementKind,
    original_style: Style,
    original_bounds: Bounds,
    handle: Handle,
    delta: Point,
    modifiers: Modifiers,
    font_size_range: [f32; 2],
) -> (ElementKind, Style, Bounds) {
    let ElementKind::Text {
        origin: _,
        content,
        scale,
    } = original
    else {
        return (original.clone(), original_style, original_bounds);
    };
    let Some(direction) = resize_direction(handle) else {
        return (original.clone(), original_style, original_bounds);
    };
    let original_size = Point::new(
        (original_bounds.max.x - original_bounds.min.x).max(f32::EPSILON),
        (original_bounds.max.y - original_bounds.min.y).max(f32::EPSILON),
    );
    let multiplier = if modifiers.alt { 2.0 } else { 1.0 };
    let dragged_size = Point::new(
        original_size.x + delta.x * direction.x * multiplier,
        original_size.y + delta.y * direction.y * multiplier,
    );
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
        crate::render::text_padding(style.size)
    } else {
        [0.0, 0.0]
    };
    let origin = text_origin(bounds, scale, padding);
    (
        ElementKind::Text {
            origin,
            content: content.clone(),
            scale,
        },
        style,
        bounds,
    )
}

fn resize_direction(handle: Handle) -> Option<Point> {
    Some(match handle {
        Handle::Corner(Corner::TopLeft) => Point::new(-1.0, -1.0),
        Handle::Corner(Corner::TopRight) => Point::new(1.0, -1.0),
        Handle::Corner(Corner::BottomRight) => Point::new(1.0, 1.0),
        Handle::Corner(Corner::BottomLeft) => Point::new(-1.0, 1.0),
        Handle::Edge(Edge::Top) => Point::new(0.0, -1.0),
        Handle::Edge(Edge::Right) => Point::new(1.0, 0.0),
        Handle::Edge(Edge::Bottom) => Point::new(0.0, 1.0),
        Handle::Edge(Edge::Left) => Point::new(-1.0, 0.0),
        Handle::Start | Handle::End | Handle::Vertex(_) => return None,
    })
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
    original_min: Point,
    original_max: Point,
    handle: Handle,
    delta: Point,
    modifiers: Modifiers,
) -> (Point, Point) {
    let center = original_min.midpoint(original_max);
    let point = handle_position(original_min, original_max, handle) + delta;
    if let Handle::Corner(corner) = handle {
        let anchor = if modifiers.alt {
            center
        } else {
            opposite_corner(original_min, original_max, corner)
        };
        return constrained_box(anchor, point, modifiers.shift, modifiers.alt);
    }

    let Handle::Edge(edge) = handle else {
        return (original_min, original_max);
    };
    let (mut min, mut max) = (original_min, original_max);
    match (edge, modifiers.alt) {
        (Edge::Top | Edge::Bottom, true) => {
            let radius = (point.y - center.y).abs();
            min.y = center.y - radius;
            max.y = center.y + radius;
        }
        (Edge::Left | Edge::Right, true) => {
            let radius = (point.x - center.x).abs();
            min.x = center.x - radius;
            max.x = center.x + radius;
        }
        (Edge::Top, false) => (min.y, max.y) = ordered(point.y, original_max.y),
        (Edge::Right, false) => (min.x, max.x) = ordered(point.x, original_min.x),
        (Edge::Bottom, false) => (min.y, max.y) = ordered(point.y, original_min.y),
        (Edge::Left, false) => (min.x, max.x) = ordered(point.x, original_max.x),
    }
    if modifiers.shift {
        match edge {
            Edge::Top | Edge::Bottom => {
                let half = (max.y - min.y) * 0.5;
                min.x = center.x - half;
                max.x = center.x + half;
            }
            Edge::Left | Edge::Right => {
                let half = (max.x - min.x) * 0.5;
                min.y = center.y - half;
                max.y = center.y + half;
            }
        }
    }
    (min, max)
}

fn handle_position(min: Point, max: Point, handle: Handle) -> Point {
    match handle {
        Handle::Corner(Corner::TopLeft) => min,
        Handle::Corner(Corner::TopRight) => Point::new(max.x, min.y),
        Handle::Corner(Corner::BottomRight) => max,
        Handle::Corner(Corner::BottomLeft) => Point::new(min.x, max.y),
        Handle::Edge(Edge::Top) => Point::new((min.x + max.x) * 0.5, min.y),
        Handle::Edge(Edge::Right) => Point::new(max.x, (min.y + max.y) * 0.5),
        Handle::Edge(Edge::Bottom) => Point::new((min.x + max.x) * 0.5, max.y),
        Handle::Edge(Edge::Left) => Point::new(min.x, (min.y + max.y) * 0.5),
        Handle::Start | Handle::End | Handle::Vertex(_) => unreachable!(),
    }
}

fn opposite_corner(min: Point, max: Point, corner: Corner) -> Point {
    match corner {
        Corner::TopLeft => max,
        Corner::TopRight => Point::new(min.x, max.y),
        Corner::BottomRight => min,
        Corner::BottomLeft => Point::new(max.x, min.y),
    }
}

fn ordered(first: f32, second: f32) -> (f32, f32) {
    (first.min(second), first.max(second))
}

pub(super) fn constrained_endpoint(start: Point, end: Point, snap: bool) -> Point {
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

pub(super) fn triangle_from_drag(start: Point, current: Point, modifiers: Modifiers) -> [Point; 3] {
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

pub(super) fn constrained_box(
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
    let end = start.translated(delta);
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
