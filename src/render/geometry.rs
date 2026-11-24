use kurbo::{Affine, BezPath, ParamCurve, ParamCurveNearest, PathSeg, Rect, Shape, Stroke};
use peniko::Fill;

use crate::config::SvgIcon;

#[derive(Debug, Clone)]
pub enum Paint {
    Fill(Fill),
    Stroke(Stroke),
}

impl From<Fill> for Paint {
    fn from(rule: Fill) -> Self {
        Self::Fill(rule)
    }
}

impl From<Stroke> for Paint {
    fn from(stroke: Stroke) -> Self {
        Self::Stroke(stroke)
    }
}

#[derive(Debug, Clone)]
pub(super) struct DrawCommand {
    pub path: BezPath,
    pub paint: Paint,
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub(super) commands: Vec<DrawCommand>,
}

pub struct LocalGeometry<'a> {
    pub(super) geometry: Geometry,
    pub(super) svg_icons: Vec<(&'a SvgIcon, kurbo::Vec2)>,
    pub(super) origin: [f32; 2],
    pub(super) size: [u32; 2],
}

impl<'a> LocalGeometry<'a> {
    pub fn new(
        geometry: Geometry,
        svg_icons: Vec<(&'a SvgIcon, kurbo::Vec2)>,
        origin: [f32; 2],
        size: [u32; 2],
    ) -> Self {
        Self {
            geometry,
            svg_icons,
            origin,
            size,
        }
    }
}

impl Geometry {
    pub fn bounds(&self) -> Option<kurbo::Rect> {
        self.commands
            .iter()
            .map(|DrawCommand { path, paint, .. }| match paint {
                Paint::Fill(_) => path.control_box(),
                Paint::Stroke(stroke) => {
                    let expansion = stroke.width * 0.5 * stroke.miter_limit.max(1.0);
                    path.control_box().inflate(expansion, expansion)
                }
            })
            .reduce(|first, second| first.union(second))
    }

    pub fn new(path: BezPath, paint: impl Into<Paint>, color: [f32; 4]) -> Self {
        let mut geometry = Self::default();
        geometry.push(path, paint, color);
        geometry
    }

    pub fn push(&mut self, path: BezPath, paint: impl Into<Paint>, color: [f32; 4]) {
        let paint = paint.into();
        self.commands.push(DrawCommand { path, paint, color });
    }

    pub fn append(&mut self, other: Self) {
        self.commands.extend(other.commands);
    }

    pub fn set_color(&mut self, color: [f32; 4]) {
        for command in &mut self.commands {
            command.color = color;
        }
    }

    pub fn swept_hit_test(&self, line: kurbo::Line, radius: f64) -> bool {
        let delta = line.p1 - line.p0;
        let length = delta.hypot();
        let to_local =
            Affine::rotate(-delta.y.atan2(delta.x)) * Affine::translate(-line.p0.to_vec2());
        self.commands.iter().any(|DrawCommand { path, paint, .. }| {
            let (tolerance, fill_rule) = match paint {
                Paint::Fill(rule) => (radius, Some(rule)),
                Paint::Stroke(stroke) => (radius + stroke.width * 0.5, None),
            };
            let bounds = line.bounding_box().inflate(tolerance, tolerance);
            let hit_edge = path.segments().any(|segment| {
                let other = control_box(segment);
                bounds.overlaps(other)
                    && if length == 0.0 {
                        segment.nearest(line.p0, 0.1).distance_sq <= tolerance * tolerance
                    } else {
                        let local = to_local * segment;
                        // A closest pair lies at an endpoint, a perpendicular extremum,
                        // or an intersection. Curve parameter tolerances are not pixels.
                        !segment.intersect_line(line).is_empty()
                            || kurbo::ParamCurveExtrema::extrema(&local)
                                .into_iter()
                                .chain([0.0, 1.0])
                                .any(|t| {
                                    let point = local.eval(t);
                                    point.y.powi(2) + (point.x - point.x.clamp(0.0, length)).powi(2)
                                        <= tolerance * tolerance
                                })
                            || [kurbo::Point::ZERO, kurbo::Point::new(length, 0.0)]
                                .into_iter()
                                .any(|point| {
                                    local.nearest(point, 0.1).distance_sq <= tolerance * tolerance
                                })
                    }
            });
            hit_edge || fill_rule.is_some_and(|rule| contains(path, *rule, line.p0))
        })
    }
}

fn contains(path: &BezPath, fill_rule: Fill, point: kurbo::Point) -> bool {
    let winding = path.winding(point);
    match fill_rule {
        Fill::NonZero => winding != 0,
        Fill::EvenOdd => winding % 2 != 0,
    }
}

/// Contains the segment like its exact bounding box, without solving for extrema.
fn control_box(segment: PathSeg) -> Rect {
    match segment {
        PathSeg::Line(line) => Rect::from_points(line.p0, line.p1),
        PathSeg::Quad(quad) => Rect::from_points(quad.p0, quad.p2).union_pt(quad.p1),
        PathSeg::Cubic(cubic) => Rect::from_points(cubic.p0, cubic.p3)
            .union_pt(cubic.p1)
            .union_pt(cubic.p2),
    }
}
