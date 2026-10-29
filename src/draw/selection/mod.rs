mod transform;

pub(super) use transform::{
    constrained_box, constrained_endpoint, resize, resize_text, triangle_from_drag,
};

use super::scene::{Bounds, ElementKind, Point, Style, geometry, rendered_segment_endpoints};
use crate::render::Geometry;
use peniko::Fill;

const ENDPOINT_HIT_RADIUS: f32 = 9.0;
const OUTLINE_HIT_RADIUS: f32 = 5.0;
const VISUAL_RADIUS: f32 = 4.5;
const SELECTION_WIDTH: f32 = 1.5;
const GAP: f32 = 4.0;
const STYLE: Style = Style {
    size: SELECTION_WIDTH,
    color: [0.1, 0.75, 1.0, 0.8],
    roundness: 0.0,
    filled: false,
};
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
            rendered_segment_endpoints(kind, style).and_then(|[start, end]| {
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
        STYLE,
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
    if let Some([start, end]) = rendered_segment_endpoints(kind, style) {
        output.extend([endpoint_geometry(start), endpoint_geometry(end)]);
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
        Fill::NonZero,
        HANDLE_FILL,
    );
    let radius = VISUAL_RADIUS - SELECTION_WIDTH * 0.5;
    output.append(geometry(
        &ElementKind::Ellipse {
            center,
            radii: Point::new(radius, radius),
        },
        STYLE,
    ));
    output
}
