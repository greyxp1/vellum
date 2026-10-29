use super::scene::{Bounds, Point, Style};
use crate::render::Geometry;
use kurbo::{BezPath, Shape};
use peniko::Fill;

pub(super) fn geometry(vertices: &[Point; 3], style: Style) -> Geometry {
    if style.size <= f32::EPSILON {
        return Geometry::default();
    }
    Geometry::fill(
        outline(vertices, style.size, style.roundness, style.filled),
        Fill::EvenOdd,
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

fn outline(vertices: &[Point; 3], width: f32, roundness: f32, filled: bool) -> BezPath {
    let outer = vertices.map(|point| kurbo::Point::new(point.x as f64, point.y as f64));
    let area = (outer[1] - outer[0]).cross(outer[2] - outer[0]);
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
        && let Some(inner) = inset_triangle(&normalized, width as f64)
    {
        add_contour(&mut path, &inner, roundness as f64);
    }
    path
}

fn add_contour(path: &mut BezPath, vertices: &[kurbo::Point; 3], roundness: f64) {
    let corners = vertices.iter().enumerate().map(|(index, &vertex)| {
        let (before, after) = super::scene::rounded_corner(
            vertices[(index + 2) % 3],
            vertex,
            vertices[(index + 1) % 3],
            roundness,
        );
        [before, vertex, after]
    });
    super::scene::append_rounded_contour(path, corners);
}

pub(super) fn rendered_vertices(vertices: &[Point; 3], roundness: f32) -> [Point; 3] {
    std::array::from_fn(|index| super::scene::rounded_tip(vertices, index, roundness))
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

fn inset_triangle(vertices: &[kurbo::Point; 3], width: f64) -> Option<[kurbo::Point; 3]> {
    let mut lines = [(kurbo::Point::ZERO, kurbo::Vec2::ZERO); 3];
    let mut normals = [kurbo::Vec2::ZERO; 3];
    for index in 0..vertices.len() {
        let edge = vertices[(index + 1) % 3] - vertices[index];
        let length = edge.hypot();
        if length <= f64::EPSILON {
            return None;
        }
        let normal = kurbo::Vec2::new(-edge.y / length, edge.x / length);
        normals[index] = normal;
        lines[index] = (vertices[index] + normal * width, edge);
    }

    let mut inner = [kurbo::Point::ZERO; 3];
    for index in 0..inner.len() {
        let previous = (index + inner.len() - 1) % inner.len();
        let (start, direction) = lines[previous];
        let (other_start, other_direction) = lines[index];
        let denominator = direction.cross(other_direction);
        if denominator.abs() <= f64::EPSILON {
            return None;
        }
        inner[index] =
            start + direction * ((other_start - start).cross(other_direction) / denominator);
    }

    let tolerance = width.max(1.0) * 1e-7;
    let area = (inner[1] - inner[0]).cross(inner[2] - inner[0]);
    (area > tolerance * tolerance
        && inner.iter().all(|point| {
            normals
                .iter()
                .enumerate()
                .all(|(index, normal)| (*point - vertices[index]).dot(*normal) >= width - tolerance)
        }))
    .then_some(inner)
}
