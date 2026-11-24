mod transform;

pub(super) use transform::{
    constrained_box, constrained_endpoint, resize, resize_text, triangle_from_drag,
};

use super::scene::{
    Bounds, Element, ElementKind, Point, Style, geometry, rendered_segment_endpoints,
};
use crate::render::Geometry;
use peniko::Fill;

const ENDPOINT_HIT_RADIUS: f32 = 9.0;
const OUTLINE_HIT_RADIUS: f32 = 5.0;
const VISUAL_RADIUS: f32 = 4.5;
const SELECTION_WIDTH: f32 = 1.5;
const GAP: f32 = 4.0;
/// How far selection outlines extend past their element.
pub(super) const OUTLINE_EXTENT: f32 = GAP + SELECTION_WIDTH;
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
    /// A side or corner of the outline, with -1, 0 or 1 for each axis.
    Outline(i8, i8),
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

pub(super) fn cursor(handle: Handle) -> CursorHint {
    match handle {
        Handle::Outline(0, _) => CursorHint::NsResize,
        Handle::Outline(_, 0) => CursorHint::EwResize,
        Handle::Outline(x, y) if x == y => CursorHint::NwseResize,
        Handle::Outline(..) => CursorHint::NeswResize,
        Handle::Start | Handle::End | Handle::Vertex(_) => CursorHint::Crosshair,
    }
}

/// Draggable points at their rendered positions in drawing order.
fn point_handles(kind: &ElementKind, style: Style) -> impl Iterator<Item = (Handle, Point)> {
    let vertices = match kind {
        ElementKind::Triangle { vertices } => Some(super::triangle::rendered_vertices(
            vertices,
            style.roundness,
        )),
        _ => None,
    };
    let vertices = vertices.into_iter().flatten().enumerate();
    let endpoints = rendered_segment_endpoints(kind, style)
        .into_iter()
        .flat_map(|[start, end]| [(Handle::Start, start), (Handle::End, end)]);
    vertices
        .map(|(index, vertex)| (Handle::Vertex(index), vertex))
        .chain(endpoints)
}

pub(super) fn hit_handle(element: &Element, point: Point) -> Option<Handle> {
    point_handles(&element.kind, element.style)
        .map(|(handle, center)| (handle, center.distance_squared(point)))
        .filter(|(_, distance)| *distance <= ENDPOINT_HIT_RADIUS * ENDPOINT_HIT_RADIUS)
        // Handles drawn later sit on top, so they win ties.
        .reduce(|best, next| if next.1 <= best.1 { next } else { best })
        .map(|(handle, _)| handle)
        .or_else(|| outline_handle(element, point))
}

fn outline_handle(element: &Element, point: Point) -> Option<Handle> {
    if !matches!(
        element.kind,
        ElementKind::Rectangle { .. } | ElementKind::Ellipse { .. } | ElementKind::Text { .. }
    ) {
        return None;
    }
    let outline = element.bounds.expanded(GAP);
    if !outline.expanded(OUTLINE_HIT_RADIUS).contains(point) {
        return None;
    }
    let axis = |value: f32, min: f32, max: f32| {
        let (low, high) = ((value - min).abs(), (value - max).abs());
        if low.min(high) > OUTLINE_HIT_RADIUS {
            0
        } else if low < high {
            -1
        } else {
            1
        }
    };
    let x = axis(point.x, outline.min.x, outline.max.x);
    let y = axis(point.y, outline.min.y, outline.max.y);
    (x != 0 || y != 0).then_some(Handle::Outline(x, y))
}

pub(super) fn outline(bounds: Bounds) -> Geometry {
    let Bounds { min, max } = bounds.expanded(GAP);
    geometry(&ElementKind::Rectangle { min, max }, STYLE)
}

pub(super) fn append_handles(kind: &ElementKind, style: Style, output: &mut Vec<Geometry>) {
    output.extend(point_handles(kind, style).map(|(_, center)| endpoint_geometry(center)));
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
