use super::freehand;
use crate::render::{FillRule, Geometry, StrokeStyle};

pub(super) const HIT_SLOP: f32 = 5.0;
const POLYGON_CORNER_INSET: f32 = 0.3;

pub type ElementId = u64;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub(super) fn distance_squared(self, other: Self) -> f32 {
        (self.x - other.x).powi(2) + (self.y - other.y).powi(2)
    }

    pub(super) fn length(self) -> f32 {
        self.x.hypot(self.y)
    }

    pub(super) fn midpoint(self, other: Self) -> Self {
        (self + other) * 0.5
    }

    pub(super) fn translated(self, delta: Self) -> Self {
        Self::new(self.x + delta.x, self.y + delta.y)
    }
}

impl std::ops::Sub for Point {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl std::ops::Add for Point {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Mul<f32> for Point {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self::Output {
        Self::new(self.x * rhs, self.y * rhs)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bounds {
    pub min: Point,
    pub max: Point,
}

impl Bounds {
    fn from_points(points: impl IntoIterator<Item = Point>) -> Self {
        let mut points = points.into_iter();
        let Some(first) = points.next() else {
            return Self::default();
        };
        let mut bounds = Self {
            min: first,
            max: first,
        };
        for point in points {
            bounds.min.x = bounds.min.x.min(point.x);
            bounds.min.y = bounds.min.y.min(point.y);
            bounds.max.x = bounds.max.x.max(point.x);
            bounds.max.y = bounds.max.y.max(point.y);
        }
        bounds
    }

    pub(super) fn expanded(self, amount: f32) -> Self {
        Self {
            min: Point::new(self.min.x - amount, self.min.y - amount),
            max: Point::new(self.max.x + amount, self.max.y + amount),
        }
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub width: f32,
    pub color: [f32; 4],
    pub roundness: f32,
    pub filled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndMarker {
    Arrow,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ElementKind {
    Path {
        points: Vec<Point>,
        smooth: bool,
        end_marker: Option<EndMarker>,
    },
    Triangle {
        vertices: [Point; 3],
    },
    Rectangle {
        min: Point,
        max: Point,
    },
    Ellipse {
        center: Point,
        radii: Point,
    },
    Text {
        origin: Point,
        content: String,
        font_size: f32,
    },
}

impl ElementKind {
    pub(super) fn translated(&self, delta: Point) -> Self {
        let mut translated = self.clone();
        match &mut translated {
            Self::Path { points, .. } => {
                points
                    .iter_mut()
                    .for_each(|point| *point = point.translated(delta));
            }
            Self::Triangle { vertices } => vertices
                .iter_mut()
                .for_each(|point| *point = point.translated(delta)),
            Self::Rectangle { min, max } => {
                *min = min.translated(delta);
                *max = max.translated(delta);
            }
            Self::Ellipse { center, .. } => *center = center.translated(delta),
            Self::Text { origin, .. } => *origin = origin.translated(delta),
        }
        translated
    }
}

#[derive(Debug)]
pub struct Element {
    pub id: ElementId,
    pub kind: ElementKind,
    pub style: Style,
    pub bounds: Bounds,
    pub geometry: Geometry,
}

impl Element {
    pub(super) fn new(id: ElementId, kind: ElementKind, style: Style) -> Self {
        let geometry = geometry(&kind, style);
        Self::with_geometry(id, kind, style, geometry)
    }

    pub(super) fn with_geometry(
        id: ElementId,
        kind: ElementKind,
        style: Style,
        geometry: Geometry,
    ) -> Self {
        let bounds = bounds_for(&kind, style);
        Self {
            id,
            kind,
            style,
            bounds,
            geometry,
        }
    }

    pub(super) fn replace(&mut self, kind: ElementKind, style: Style) -> (ElementKind, Style) {
        let kind = std::mem::replace(&mut self.kind, kind);
        let style = std::mem::replace(&mut self.style, style);
        self.bounds = bounds_for(&self.kind, self.style);
        self.geometry = geometry(&self.kind, self.style);
        (kind, style)
    }

    pub(super) fn update_text_bounds(&mut self, [width, height]: [f32; 2]) {
        let ElementKind::Text { origin, .. } = self.kind else {
            return;
        };
        let half_width = self.style.width * 0.5;
        self.bounds = Bounds {
            min: origin,
            max: Point::new(origin.x + width, origin.y + height),
        }
        .expanded(half_width);
    }

    pub(super) fn preview_bounds(&self, kind: &ElementKind) -> Bounds {
        match (&self.kind, kind) {
            (
                ElementKind::Text { origin, .. },
                ElementKind::Text {
                    origin: preview, ..
                },
            ) => {
                let delta = *preview - *origin;
                Bounds {
                    min: self.bounds.min.translated(delta),
                    max: self.bounds.max.translated(delta),
                }
            }
            _ => bounds_for(kind, self.style),
        }
    }

    pub(super) fn hit_test(&self, point: Point) -> bool {
        self.hit_test_with_slop(point, HIT_SLOP, false)
    }

    pub(super) fn erase_hit_test(&self, point: Point, radius: f32) -> bool {
        self.hit_test_with_slop(point, radius, true)
    }

    fn hit_test_with_slop(&self, point: Point, slop: f32, expand_text: bool) -> bool {
        if !self.bounds.expanded(slop).contains(point) {
            return false;
        }
        let tolerance = self.style.width * 0.5 + slop;
        match &self.kind {
            ElementKind::Path {
                points,
                smooth,
                end_marker,
            } => {
                if matches!(end_marker, Some(EndMarker::Arrow))
                    && let Some((start, end)) = path_endpoints(points)
                {
                    arrow_hit_test(
                        arrow_head(start, end, self.style.width),
                        self.style,
                        point,
                        slop,
                    )
                } else {
                    if *smooth {
                        freehand::hit_test(points, self.style, point, slop)
                    } else {
                        polyline_hit(points, point, tolerance)
                    }
                }
            }
            ElementKind::Triangle { vertices } => {
                super::triangle::hit_test(vertices, self.style, point, slop)
            }
            ElementKind::Rectangle { min, max } => rounded_rectangle_hit(
                *min,
                *max,
                self.style.roundness,
                self.style.filled,
                point,
                tolerance,
            ),
            ElementKind::Ellipse { center, radii } => {
                if radii.x <= f32::EPSILON || radii.y <= f32::EPSILON {
                    return point.distance_squared(*center) <= tolerance.powi(2);
                }
                let local = point - *center;
                let normalized = ((local.x / radii.x).powi(2) + (local.y / radii.y).powi(2)).sqrt();
                let normalized_tolerance = tolerance / radii.x.min(radii.y).max(1.0);
                if self.style.filled {
                    normalized <= 1.0 + normalized_tolerance
                } else {
                    (normalized - 1.0).abs() <= normalized_tolerance
                }
            }
            ElementKind::Text { .. } => {
                if expand_text {
                    self.bounds.expanded(slop).contains(point)
                } else {
                    self.bounds.contains(point)
                }
            }
        }
    }
}

pub(super) fn bounds_for(kind: &ElementKind, style: Style) -> Bounds {
    if let ElementKind::Triangle { vertices } = kind {
        return super::triangle::bounds(vertices, style);
    }
    let width = style.width;
    let bounds = match kind {
        ElementKind::Path {
            points,
            end_marker: Some(EndMarker::Arrow),
            ..
        } => path_endpoints(points).map_or_else(
            || Bounds::from_points(points.iter().copied()),
            |(start, end)| {
                let head = arrow_head(start, end, width);
                Bounds::from_points(points.iter().copied().chain(head.vertices))
            },
        ),
        ElementKind::Path { points, .. } => Bounds::from_points(points.iter().copied()),
        ElementKind::Triangle { .. } => unreachable!(),
        ElementKind::Rectangle { min, max } => Bounds {
            min: *min,
            max: *max,
        },
        ElementKind::Ellipse { center, radii } => Bounds {
            min: Point::new(center.x - radii.x, center.y - radii.y),
            max: Point::new(center.x + radii.x, center.y + radii.y),
        },
        ElementKind::Text {
            origin,
            content,
            font_size,
        } => Bounds {
            min: *origin,
            max: Point::new(
                origin.x + content.chars().count().max(1) as f32 * font_size * 0.65,
                origin.y + font_size * 1.25,
            ),
        },
    };
    let radius = width * 0.5;
    let expansion = if matches!(kind, ElementKind::Path { smooth: true, .. }) {
        let roundness = style.roundness.clamp(0.0, 1.0);
        radius * (std::f32::consts::SQRT_2 - (std::f32::consts::SQRT_2 - 1.0) * roundness)
    } else {
        radius
    };
    bounds.expanded(expansion)
}

pub(super) fn geometry(kind: &ElementKind, style: Style) -> Geometry {
    use kurbo::Shape;

    if let ElementKind::Path {
        points,
        smooth: true,
        ..
    } = kind
    {
        return freehand::geometry(points, style);
    }
    if let ElementKind::Rectangle { min, max } = kind {
        return rectangle_geometry(*min, *max, style);
    }
    if let ElementKind::Triangle { vertices } = kind {
        return super::triangle::geometry(vertices, style);
    }
    if let ElementKind::Ellipse { center, radii } = kind
        && style.filled
    {
        let half = style.width * 0.5;
        let path = kurbo::Ellipse::new(
            (f64::from(center.x), f64::from(center.y)),
            (
                f64::from((radii.x + half).max(0.0)),
                f64::from((radii.y + half).max(0.0)),
            ),
            0.0,
        )
        .to_path(0.1);
        return Geometry::fill(path, FillRule::NonZero, style.color);
    }
    let marker = match kind {
        ElementKind::Path {
            points,
            end_marker: Some(EndMarker::Arrow),
            ..
        } => path_endpoints(points).map(|(start, end)| arrow_head(start, end, style.width)),
        _ => None,
    };
    if let Some(head) = marker {
        return Geometry::fill(
            arrow_path(head, style.roundness),
            FillRule::NonZero,
            style.color,
        );
    }
    let mut path = kurbo::BezPath::new();
    let mut caps = Vec::new();
    match kind {
        ElementKind::Path {
            points,
            smooth: false,
            ..
        } => {
            if let [start, end] = points.as_slice() {
                let radius = style.width * 0.5;
                let start_roundness = style
                    .roundness
                    .min((*end - *start).length() / style.width.max(f32::EPSILON));
                let start_center = inset_endpoint(*start, *end, radius * start_roundness);
                let end_center = inset_endpoint(*end, *start, radius * start_roundness);

                path.move_to((f64::from(start_center.x), f64::from(start_center.y)));
                path.line_to((f64::from(end_center.x), f64::from(end_center.y)));
                caps.push((start_center, *start - *end, start_roundness));
                caps.push((end_center, *end - *start, start_roundness));
            }
        }
        ElementKind::Path { smooth: true, .. } => unreachable!(),
        ElementKind::Triangle { .. } => unreachable!(),
        ElementKind::Rectangle { .. } => unreachable!(),
        ElementKind::Ellipse { center, radii } => {
            path = kurbo::Ellipse::new(
                (f64::from(center.x), f64::from(center.y)),
                (f64::from(radii.x), f64::from(radii.y)),
                0.0,
            )
            .to_path(0.1);
        }
        ElementKind::Text { .. } => return Geometry::empty(),
    }

    let mut geometry =
        Geometry::stroke(path, StrokeStyle::new(f64::from(style.width)), style.color);
    for (center, outward, roundness) in caps {
        geometry.append(freehand::rounded_cap(
            center,
            outward,
            style.width * 0.5,
            roundness,
            style.color,
        ));
    }
    geometry
}

pub(super) fn rendered_path_endpoints(kind: &ElementKind, style: Style) -> Option<[Point; 2]> {
    let ElementKind::Path {
        points,
        smooth: false,
        end_marker,
    } = kind
    else {
        return None;
    };
    let [first, last] = points.as_slice() else {
        return None;
    };
    let end = if matches!(end_marker, Some(EndMarker::Arrow)) {
        arrow_head(*first, *last, style.width).rendered_tip(style.roundness)
    } else {
        *last
    };
    Some([*first, end])
}

fn inset_endpoint(endpoint: Point, neighbor: Point, amount: f32) -> Point {
    let inward = neighbor - endpoint;
    let length = inward.length();
    if length <= f32::EPSILON {
        endpoint
    } else {
        endpoint + inward * (amount / length)
    }
}

fn rectangle_radius(min: Point, max: Point, roundness: f32) -> f32 {
    ((max.x - min.x).abs().min((max.y - min.y).abs()) * 0.5) * roundness
}

fn rectangle_geometry(min: Point, max: Point, style: Style) -> Geometry {
    use kurbo::Shape;

    let half = style.width * 0.5;
    let maximum = (max.x - min.x).abs().min((max.y - min.y).abs()) * 0.5;
    let outer_radius = if style.roundness <= f32::EPSILON {
        0.0
    } else {
        half + maximum * style.roundness
    };
    let contours = [
        (
            Point::new(min.x - half, min.y - half),
            Point::new(max.x + half, max.y + half),
            outer_radius,
        ),
        (
            Point::new(min.x + half, min.y + half),
            Point::new(max.x - half, max.y - half),
            (maximum - half).max(0.0) * style.roundness,
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
    Geometry::fill(path, FillRule::EvenOdd, style.color)
}

fn rounded_rectangle_hit(
    min: Point,
    max: Point,
    roundness: f32,
    filled: bool,
    point: Point,
    tolerance: f32,
) -> bool {
    let radius = rectangle_radius(min, max, roundness);
    let center = min.midpoint(max);
    let x = (point.x - center.x).abs() - ((max.x - min.x) * 0.5 - radius);
    let y = (point.y - center.y).abs() - ((max.y - min.y) * 0.5 - radius);
    let distance = x.max(0.0).hypot(y.max(0.0)) + x.max(y).min(0.0) - radius;
    if filled {
        distance <= tolerance
    } else {
        distance.abs() <= tolerance
    }
}

pub(super) fn default_roundness(kind: &ElementKind) -> Option<f32> {
    use super::tool::Tool;

    match kind {
        ElementKind::Path { smooth: true, .. } => Tool::Pen.default_roundness(),
        ElementKind::Path {
            end_marker: Some(EndMarker::Arrow),
            ..
        } => Tool::Arrow.default_roundness(),
        ElementKind::Path { .. } => Tool::Line.default_roundness(),
        ElementKind::Triangle { .. } => Tool::Triangle.default_roundness(),
        ElementKind::Rectangle { .. } => Tool::Rectangle.default_roundness(),
        _ => None,
    }
}

fn polyline_hit(points: &[Point], point: Point, tolerance: f32) -> bool {
    let tolerance_squared = tolerance * tolerance;
    match points {
        [] => false,
        [only] => only.distance_squared(point) <= tolerance_squared,
        _ => points.windows(2).any(|segment| {
            segment_distance_squared(point, segment[0], segment[1]) <= tolerance_squared
        }),
    }
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

impl ArrowHead {
    fn rendered_tip(self, roundness: f32) -> Point {
        let (before, after) = rounded_polygon_corner(&self.vertices, 0, roundness);
        (before + self.vertices[0] * 2.0 + after) * 0.25
    }
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

fn rounded_polygon_path(vertices: &[Point], roundness: f32) -> kurbo::BezPath {
    let mut path = kurbo::BezPath::new();
    if vertices.len() < 3 {
        return path;
    }
    let mut corner = rounded_polygon_corner(vertices, 0, roundness);
    path.move_to(kurbo_point(corner.0));
    for index in 0..vertices.len() {
        let vertex = vertices[index];
        let (before, after) = corner;
        if before == after {
            path.line_to(kurbo_point(vertex));
        } else {
            path.quad_to(kurbo_point(vertex), kurbo_point(after));
        }
        if index + 1 < vertices.len() {
            corner = rounded_polygon_corner(vertices, index + 1, roundness);
            path.line_to(kurbo_point(corner.0));
        }
    }
    path.close_path();
    path
}

fn rounded_polygon_corner(vertices: &[Point], index: usize, roundness: f32) -> (Point, Point) {
    let vertex = vertices[index];
    let roundness = roundness.clamp(0.0, 1.0);
    if roundness <= f32::EPSILON {
        return (vertex, vertex);
    }
    let to_previous = vertices[(index + vertices.len() - 1) % vertices.len()] - vertex;
    let to_next = vertices[(index + 1) % vertices.len()] - vertex;
    let previous_length = to_previous.length();
    let next_length = to_next.length();
    if previous_length <= f32::EPSILON || next_length <= f32::EPSILON {
        return (vertex, vertex);
    }
    let cut = POLYGON_CORNER_INSET * roundness * previous_length.min(next_length);
    (
        vertex + to_previous * (cut / previous_length),
        vertex + to_next * (cut / next_length),
    )
}

fn arrow_hit_test(head: ArrowHead, style: Style, point: Point, tolerance: f32) -> bool {
    use kurbo::{ParamCurveNearest, Shape};

    let path = arrow_path(head, style.roundness);
    let point = kurbo_point(point);
    if path.winding(point) != 0 {
        return true;
    }
    let tolerance_squared = f64::from(tolerance.max(0.0).powi(2));
    tolerance_squared > 0.0
        && path
            .segments()
            .any(|segment| segment.nearest(point, 0.1).distance_sq <= tolerance_squared)
}

fn kurbo_point(point: Point) -> kurbo::Point {
    kurbo::Point::new(f64::from(point.x), f64::from(point.y))
}

fn path_endpoints(points: &[Point]) -> Option<(Point, Point)> {
    let end = *points.last()?;
    let start = *points.get(points.len().checked_sub(2)?)?;
    Some((start, end))
}

fn segment_distance_squared(point: Point, start: Point, end: Point) -> f32 {
    let delta = end - start;
    let length_squared = delta.distance_squared(Point::default());
    if length_squared <= f32::EPSILON {
        return point.distance_squared(start);
    }
    let offset = point - start;
    let fraction = ((offset.x * delta.x + offset.y * delta.y) / length_squared).clamp(0.0, 1.0);
    let projection = Point::new(start.x + delta.x * fraction, start.y + delta.y * fraction);
    point.distance_squared(projection)
}
