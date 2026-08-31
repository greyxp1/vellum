use super::scene::{Bounds, Point, Style};
use crate::render::{FillRule, Geometry};
use kurbo::{BezPath, ParamCurveNearest, Shape};

const CORNER_INSET: f64 = 0.3;

pub(super) fn geometry(vertices: &[Point; 3], style: Style) -> Geometry {
    if style.size <= f32::EPSILON {
        return Geometry::empty();
    }
    Geometry::fill(
        outline(vertices, style.size, style.roundness, style.filled),
        FillRule::EvenOdd,
        style.color,
    )
}

pub(super) fn bounds(vertices: &[Point; 3], style: Style) -> Bounds {
    let bounds = outline(vertices, style.size, style.roundness, style.filled).bounding_box();
    Bounds {
        min: Point::new(bounds.x0 as f32, bounds.y0 as f32),
        max: Point::new(bounds.x1 as f32, bounds.y1 as f32),
    }
}

pub(super) fn hit_test(vertices: &[Point; 3], style: Style, point: Point, slop: f32) -> bool {
    let outline = outline(vertices, style.size, style.roundness, style.filled);
    let point = kurbo::Point::new(point.x as f64, point.y as f64);
    if outline.winding(point).unsigned_abs() % 2 == 1 {
        return true;
    }
    let tolerance_squared = f64::from(slop * slop);
    outline
        .segments()
        .any(|segment| segment.nearest(point, 0.1).distance_sq <= tolerance_squared)
}

fn outline(vertices: &[Point; 3], width: f32, roundness: f32, filled: bool) -> BezPath {
    let outer = vertices.map(|point| kurbo::Point::new(point.x as f64, point.y as f64));
    let area = cross(outer[1] - outer[0], outer[2] - outer[0]);
    let scale = outer
        .iter()
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0, f64::max);
    let normalized = if area.is_sign_negative() {
        [outer[0], outer[2], outer[1]]
    } else {
        outer
    };
    let mut path = BezPath::new();
    add_contour(&mut path, &normalized, roundness as f64);
    if !filled
        && area.abs() > f64::EPSILON * scale * scale * 16.0
        && let Some(inner) = inset_triangle(&normalized, width as f64, 1.0)
    {
        add_contour(&mut path, &inner, roundness as f64);
    }
    path
}

fn add_contour(path: &mut BezPath, vertices: &[kurbo::Point; 3], roundness: f64) {
    let roundness = roundness.clamp(0.0, 1.0);
    if roundness <= f64::EPSILON {
        path.move_to(vertices[0]);
        path.line_to(vertices[1]);
        path.line_to(vertices[2]);
        path.close_path();
        return;
    }

    let corners: [(kurbo::Point, kurbo::Point); 3] =
        std::array::from_fn(|index| rounded_corner(vertices, index, roundness));
    path.move_to(corners[0].0);
    for index in 0..vertices.len() {
        path.quad_to(vertices[index], corners[index].1);
        if index + 1 < vertices.len() {
            path.line_to(corners[index + 1].0);
        }
    }
    path.close_path();
}

fn rounded_corner(
    vertices: &[kurbo::Point; 3],
    index: usize,
    roundness: f64,
) -> (kurbo::Point, kurbo::Point) {
    let vertex = vertices[index];
    let to_previous = vertices[(index + 2) % 3] - vertex;
    let to_next = vertices[(index + 1) % 3] - vertex;
    let previous_length = to_previous.hypot();
    let next_length = to_next.hypot();
    if previous_length <= f64::EPSILON || next_length <= f64::EPSILON {
        return (vertex, vertex);
    }
    let cut = CORNER_INSET * roundness * previous_length.min(next_length);
    (
        vertex + to_previous * (cut / previous_length),
        vertex + to_next * (cut / next_length),
    )
}

pub(super) fn rendered_vertices(vertices: &[Point; 3], roundness: f32) -> [Point; 3] {
    let vertices = vertices.map(|point| kurbo::Point::new(point.x as f64, point.y as f64));
    std::array::from_fn(|index| {
        let (before, after) = rounded_corner(&vertices, index, roundness.clamp(0.0, 1.0) as f64);
        let vertex = vertices[index];
        Point::new(
            ((before.x + vertex.x * 2.0 + after.x) * 0.25) as f32,
            ((before.y + vertex.y * 2.0 + after.y) * 0.25) as f32,
        )
    })
}

pub(super) fn dragged_vertex(
    vertices: &[Point; 3],
    index: usize,
    delta: Point,
    roundness: f32,
) -> Point {
    let target = rendered_vertices(vertices, roundness)[index] + delta;
    let mut vertex = vertices[index] + delta;
    for _ in 0..8 {
        let mut candidate = *vertices;
        candidate[index] = vertex;
        let correction = target - rendered_vertices(&candidate, roundness)[index];
        vertex = vertex + correction;
        if correction.length() <= 0.001 {
            break;
        }
    }
    vertex
}

fn inset_triangle(
    vertices: &[kurbo::Point; 3],
    width: f64,
    orientation: f64,
) -> Option<[kurbo::Point; 3]> {
    let mut lines = [(kurbo::Point::ZERO, kurbo::Vec2::ZERO); 3];
    let mut normals = [kurbo::Vec2::ZERO; 3];
    for index in 0..vertices.len() {
        let edge = vertices[(index + 1) % 3] - vertices[index];
        let length = edge.hypot();
        if length <= f64::EPSILON {
            return None;
        }
        let normal = kurbo::Vec2::new(
            -edge.y * orientation / length,
            edge.x * orientation / length,
        );
        normals[index] = normal;
        lines[index] = (vertices[index] + normal * width, edge);
    }

    let mut inner = [kurbo::Point::ZERO; 3];
    for index in 0..inner.len() {
        let previous = (index + inner.len() - 1) % inner.len();
        let (start, direction) = lines[previous];
        let (other_start, other_direction) = lines[index];
        let denominator = cross(direction, other_direction);
        if denominator.abs() <= f64::EPSILON {
            return None;
        }
        inner[index] =
            start + direction * (cross(other_start - start, other_direction) / denominator);
    }

    let tolerance = width.max(1.0) * 1e-7;
    let area = cross(inner[1] - inner[0], inner[2] - inner[0]);
    (area * orientation > tolerance * tolerance
        && inner.iter().all(|point| {
            normals
                .iter()
                .enumerate()
                .all(|(index, normal)| (*point - vertices[index]).dot(*normal) >= width - tolerance)
        }))
    .then_some(inner)
}

fn cross(first: kurbo::Vec2, second: kurbo::Vec2) -> f64 {
    first.x * second.y - first.y * second.x
}
