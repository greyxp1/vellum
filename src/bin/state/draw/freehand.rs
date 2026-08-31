use kurbo::{BezPath, ParamCurveNearest, Shape};

use super::scene::{Point, Style, pixel_aligned_point, pixel_aligned_points};
use super::{CIRCLE_KAPPA, STABILIZER_FOLLOW, stabilizer_delay};
use crate::render::{FillRule, Geometry};

const MIN_SAMPLE_DISTANCE_SQUARED: f32 = 1.0;
const STAMP_DIRECTION_LENGTH: f32 = 0.01;
const SNAP_ANGLE: f32 = std::f32::consts::PI / 12.0;
const CHUNK_POINTS: usize = 2048;
// Keeping the centerline point and raw index lets the rolling tail resume
// perfect_freehand exactly.
#[derive(Debug)]
struct CacheAnchor {
    centerline: [f64; 2],
    raw_index: usize,
}

#[derive(Debug)]
pub(super) struct LiveStroke {
    points: Vec<Point>,
    sample_anchor: Point,
    sample_pending: bool,
    stabilized_point: Point,
    alignment_offset: Point,
    direction_locked: bool,
    style: Style,
    cache_anchor: Option<CacheAnchor>,
    cached: Vec<BezPath>,
}

impl LiveStroke {
    pub fn new(point: Point, style: Style) -> Self {
        let aligned = pixel_aligned_point(point, style.size);
        Self {
            points: vec![aligned],
            sample_anchor: aligned,
            sample_pending: false,
            stabilized_point: aligned,
            alignment_offset: aligned - point,
            direction_locked: false,
            style,
            cache_anchor: None,
            cached: Vec::new(),
        }
    }

    pub fn push(&mut self, point: Point, snap: bool) -> bool {
        let point = point + self.alignment_offset;
        if !self.direction_locked {
            if !self.direction_is_ready(point) {
                return false;
            }
            self.commit_direction(self.oriented_point(point, snap));
            return true;
        }

        let offset = point - self.stabilized_point;
        let distance = offset.x.hypot(offset.y);
        let delay = stabilizer_delay(self.style.size);
        if distance <= delay {
            return false;
        }
        let target = point - offset * (delay / distance);
        self.stabilized_point =
            self.stabilized_point + (target - self.stabilized_point) * STABILIZER_FOLLOW;
        let changed = push(
            &mut self.points,
            &mut self.sample_anchor,
            &mut self.sample_pending,
            self.stabilized_point,
        );
        if changed {
            self.cache_ready_chunk();
        }
        changed
    }

    pub fn tail_geometry(&self) -> Geometry {
        if self.direction_locked {
            self.render_tail(false)
        } else {
            render_geometry(&self.points, self.style, false)
        }
    }

    pub fn update_style(&mut self, style: Style) {
        if self.style == style {
            return;
        }
        if self.style.size != style.size {
            let raw_start = self.points[0] - self.alignment_offset;
            let offset = pixel_aligned_point(raw_start, style.size) - self.points[0];
            self.points
                .iter_mut()
                .for_each(|point| *point = point.translated(offset));
            self.sample_anchor = self.sample_anchor.translated(offset);
            self.stabilized_point = self.stabilized_point.translated(offset);
            self.alignment_offset = self.alignment_offset + offset;
        }
        self.style = style;
        self.cache_anchor = None;
        self.cached.clear();
        while self.cache_ready_chunk() {}
    }

    pub fn finish(mut self, point: Point, snap: bool) -> (Vec<Point>, Style, Geometry) {
        let point = point + self.alignment_offset;
        if self.direction_locked {
            // Keep the endpoint shown by the stabilized live preview.
            if let Some([x, y]) = self.tail_centerline(false).last().copied() {
                *self
                    .points
                    .last_mut()
                    .expect("freehand starts with one point") = Point::new(x as f32, y as f32);
            }
        } else {
            let point = self.oriented_point(point, snap);
            if point != self.points[0] {
                self.commit_direction(point);
            }
        }
        if self.points.len() == 1 {
            self.points
                .push(self.points[0] + Point::new(0.0, -STAMP_DIRECTION_LENGTH));
        }
        while self.cache_ready_chunk() {}
        let geometry = self.render_tail(true);
        (self.points, self.style, geometry)
    }

    fn oriented_point(&self, point: Point, snap: bool) -> Point {
        let start = self.points[0];
        let offset = point - start;
        let distance = offset.length();
        if !snap || distance <= f32::EPSILON {
            return point;
        }
        let angle = (offset.y.atan2(offset.x) / SNAP_ANGLE).round() * SNAP_ANGLE;
        start + Point::new(angle.cos(), angle.sin()) * distance
    }

    fn direction_is_ready(&self, point: Point) -> bool {
        if self.style.roundness >= 1.0 - f32::EPSILON {
            return point != self.points[0];
        }
        let setup_distance = stabilizer_delay(self.style.size).max(12.0);
        self.points[0].distance_squared(point) >= setup_distance.powi(2)
    }

    fn commit_direction(&mut self, point: Point) {
        push(
            &mut self.points,
            &mut self.sample_anchor,
            &mut self.sample_pending,
            point,
        );
        self.sample_anchor = point;
        self.sample_pending = false;
        self.stabilized_point = point;
        self.direction_locked = true;
    }

    fn cache_ready_chunk(&mut self) -> bool {
        let raw_tail_len = self
            .cache_anchor
            .as_ref()
            .map_or(self.points.len(), |anchor| {
                self.points.len().saturating_sub(anchor.raw_index)
            });
        if raw_tail_len <= CHUNK_POINTS + 2 {
            return false;
        }
        let centerline = self.tail_centerline(false);
        if centerline.len() <= CHUNK_POINTS + 2 {
            return false;
        }

        let split = CHUNK_POINTS;
        self.cached.push(centerline_path(
            &centerline[..=split + 1],
            self.style,
            self.cache_anchor.is_some(),
            true,
        ));
        let raw_index = self.cache_anchor.as_ref().map_or_else(
            || split + self.points.len() - centerline.len(),
            |anchor| anchor.raw_index + split,
        );
        self.cache_anchor = Some(CacheAnchor {
            centerline: centerline[split],
            raw_index,
        });
        true
    }

    fn render_tail(&self, complete: bool) -> Geometry {
        if self.cache_anchor.is_none() {
            return render_geometry(&self.points, self.style, complete);
        }
        let mut path = BezPath::new();
        for cached in &self.cached {
            path.extend(cached.elements().iter().copied());
        }
        path.extend(centerline_path(
            &self.tail_centerline(complete),
            self.style,
            true,
            false,
        ));
        Geometry::fill(path, FillRule::NonZero, self.style.color)
    }

    fn tail_centerline(&self, complete: bool) -> Vec<[f64; 2]> {
        let Some(anchor) = &self.cache_anchor else {
            return centerline_points(&self.points, self.style.size, true, complete);
        };
        let mut input = Vec::with_capacity(self.points.len() - anchor.raw_index);
        input.push(perfect_freehand::InputPoint::Array(anchor.centerline, None));
        input.extend(self.points[anchor.raw_index + 1..].iter().map(|point| {
            perfect_freehand::InputPoint::Array([f64::from(point.x), f64::from(point.y)], None)
        }));
        perfect_freehand::get_stroke_points(&input, &stroke_options(0.0, complete))
            .into_iter()
            .map(|point| point.point)
            .collect()
    }
}

fn push(
    points: &mut Vec<Point>,
    sample_anchor: &mut Point,
    sample_pending: &mut bool,
    next: Point,
) -> bool {
    if points.last() == Some(&next) {
        return false;
    }

    if sample_anchor.distance_squared(next) >= MIN_SAMPLE_DISTANCE_SQUARED {
        if *sample_pending {
            *points.last_mut().expect("freehand starts with one point") = next;
        } else {
            points.push(next);
        }
        *sample_anchor = next;
        *sample_pending = false;
    } else if *sample_pending {
        if points.get(points.len().saturating_sub(2)) == Some(&next) {
            points.pop();
            *sample_pending = false;
        } else {
            *points.last_mut().expect("freehand starts with one point") = next;
        }
    } else {
        points.push(next);
        *sample_pending = true;
    }
    true
}

pub(super) fn geometry(points: &[Point], style: Style) -> Geometry {
    render_geometry(points, style, true)
}

fn render_geometry(points: &[Point], style: Style, complete: bool) -> Geometry {
    stroke_path(points, style, complete).map_or_else(Geometry::empty, |path| {
        Geometry::fill(path, FillRule::NonZero, style.color)
    })
}

pub(super) fn hit_test(points: &[Point], style: Style, point: Point, slop: f32) -> bool {
    let Some(path) = stroke_path(points, style, true) else {
        return false;
    };
    let point = kurbo::Point::new(f64::from(point.x), f64::from(point.y));
    if path.contains(point) {
        return true;
    }
    let slop_squared = f64::from(slop.max(0.0).powi(2));
    slop_squared > 0.0
        && path
            .segments()
            .any(|segment| segment.nearest(point, 0.1).distance_sq <= slop_squared)
}

fn stroke_path(points: &[Point], style: Style, complete: bool) -> Option<kurbo::BezPath> {
    let points = pixel_aligned_points(points, style.size);
    let points = points.as_ref();
    let distinct = points
        .windows(2)
        .any(|pair| pair[0] == pair[1])
        .then(|| distinct_points(points));
    let points = distinct.as_deref().unwrap_or(points);
    let &first = points.first()?;
    let radius = style.size.max(0.0) * 0.5;
    if radius <= f32::EPSILON {
        return None;
    }
    let roundness = style.roundness.clamp(0.0, 1.0);
    if points.len() == 1 {
        return Some(brush_stamp(kurbo_point(first), radius, roundness, 0.0));
    }
    if points.len() == 2 && first.distance_squared(points[1]) < MIN_SAMPLE_DISTANCE_SQUARED {
        let direction = points[1] - first;
        return Some(brush_stamp(
            kurbo_point(first),
            radius,
            roundness,
            direction.y.atan2(direction.x) + std::f32::consts::FRAC_PI_2,
        ));
    }

    Some(centerline_path(
        &centerline_points(points, style.size, true, complete),
        style,
        false,
        false,
    ))
}

fn centerline_points(
    points: &[Point],
    width: f32,
    filter_short_start: bool,
    complete: bool,
) -> Vec<[f64; 2]> {
    let input = points
        .iter()
        .map(|point| {
            perfect_freehand::InputPoint::Array([f64::from(point.x), f64::from(point.y)], None)
        })
        .collect::<Vec<_>>();
    perfect_freehand::get_stroke_points(
        &input,
        &stroke_options(if filter_short_start { width } else { 0.0 }, complete),
    )
    .into_iter()
    .map(|point| point.point)
    .collect()
}

fn stroke_options(width: f32, complete: bool) -> perfect_freehand::StrokeOptions {
    perfect_freehand::StrokeOptions {
        size: Some(f64::from(width)),
        thinning: Some(0.0),
        smoothing: Some(0.5),
        streamline: Some(0.5),
        simulate_pressure: Some(false),
        last: Some(complete),
        ..Default::default()
    }
}

fn centerline_path(
    centerline_points: &[[f64; 2]],
    style: Style,
    start_cut: bool,
    end_cut: bool,
) -> kurbo::BezPath {
    let centerline = deposited_centerline(centerline_points, start_cut, end_cut);
    let radius = style.size.max(0.0) * 0.5;
    let roundness = style.roundness.clamp(0.0, 1.0);
    let mut path = joined_swept_path(&centerline, radius);
    append_endpoint_caps(
        &mut path,
        &centerline,
        radius,
        roundness,
        start_cut,
        end_cut,
    );
    path
}

fn append_endpoint_caps(
    path: &mut kurbo::BezPath,
    centerline: &[kurbo::Point],
    radius: f32,
    roundness: f32,
    start_cut: bool,
    end_cut: bool,
) {
    if let (Some([start, second, ..]), Some([.., penultimate, end])) =
        (centerline.get(..), centerline.get(..))
    {
        if !start_cut {
            append_partial_cap(
                path,
                [start.x, start.y],
                [start.x - second.x, start.y - second.y],
                radius,
                roundness,
            );
        }
        if !end_cut {
            append_partial_cap(
                path,
                [end.x, end.y],
                [end.x - penultimate.x, end.y - penultimate.y],
                radius,
                roundness,
            );
        }
    }
}

fn deposited_centerline(points: &[[f64; 2]], start_cut: bool, end_cut: bool) -> Vec<kurbo::Point> {
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    if points.len() == 1 {
        return vec![array_point(first)];
    }

    let mut deposited = Vec::with_capacity(points.len());
    deposited.push(array_point(if start_cut {
        midpoint(points[0], points[1])
    } else {
        first
    }));
    deposited.extend(points[1..points.len() - 1].iter().copied().map(array_point));
    deposited.push(array_point(if end_cut {
        midpoint(points[points.len() - 2], points[points.len() - 1])
    } else {
        *points.last().expect("freehand centerline is non-empty")
    }));
    deposited
}

fn joined_swept_path(points: &[kurbo::Point], radius: f32) -> kurbo::BezPath {
    let mut path = kurbo::BezPath::new();
    let radius = f64::from(radius);
    for segment in points.windows(2) {
        let [start, end] = [segment[0], segment[1]];
        let offset = end - start;
        let length = offset.hypot();
        if length <= f64::EPSILON {
            continue;
        }
        let normal = kurbo::Vec2::new(-offset.y, offset.x) * (radius / length);
        path.move_to(start + normal);
        path.line_to(start - normal);
        path.line_to(end - normal);
        path.line_to(end + normal);
        path.close_path();
    }
    for corner in points.windows(3) {
        append_round_join(&mut path, corner[0], corner[1], corner[2], radius);
    }
    path
}

fn append_round_join(
    path: &mut kurbo::BezPath,
    previous: kurbo::Point,
    center: kurbo::Point,
    next: kurbo::Point,
    radius: f64,
) {
    let incoming = center - previous;
    let outgoing = next - center;
    let incoming_length = incoming.hypot();
    let outgoing_length = outgoing.hypot();
    if incoming_length <= f64::EPSILON || outgoing_length <= f64::EPSILON {
        return;
    }
    let incoming = incoming / incoming_length;
    let outgoing = outgoing / outgoing_length;
    let turn = incoming.cross(outgoing).atan2(incoming.dot(outgoing));
    if turn.abs() <= f64::EPSILON || (std::f64::consts::PI - turn.abs()) <= f64::EPSILON {
        return;
    }

    let incoming_normal = kurbo::Vec2::new(-incoming.y, incoming.x) * radius;
    let outgoing_normal = kurbo::Vec2::new(-outgoing.y, outgoing.x) * radius;
    let (start, sweep) = if turn > 0.0 {
        (-incoming_normal, turn)
    } else {
        (outgoing_normal, -turn)
    };
    path.move_to(center);
    path.line_to(center + start);
    path.extend(
        kurbo::Arc::new(
            center,
            kurbo::Vec2::new(radius, radius),
            start.y.atan2(start.x),
            sweep,
            0.0,
        )
        .append_iter(0.1),
    );
    path.close_path();
}

fn midpoint(first: [f64; 2], second: [f64; 2]) -> [f64; 2] {
    [(first[0] + second[0]) * 0.5, (first[1] + second[1]) * 0.5]
}

fn array_point([x, y]: [f64; 2]) -> kurbo::Point {
    kurbo::Point::new(x, y)
}

fn distinct_points(points: &[Point]) -> Vec<Point> {
    let mut distinct = Vec::with_capacity(points.len());
    for &point in points {
        push_distinct(&mut distinct, point);
    }
    distinct
}

fn brush_stamp(center: kurbo::Point, radius: f32, roundness: f32, angle: f32) -> kurbo::BezPath {
    let radius = f64::from(radius);
    let roundness = f64::from(roundness);
    let corner_radius = radius * roundness;
    let mut path =
        kurbo::RoundedRect::new(-radius, -radius, radius, radius, corner_radius).to_path(0.1);
    path.apply_affine(
        kurbo::Affine::rotate(f64::from(angle))
            .then_translate(kurbo::Vec2::new(center.x, center.y)),
    );
    path
}

fn append_partial_cap(
    path: &mut kurbo::BezPath,
    center: [f64; 2],
    outward: [f64; 2],
    radius: f32,
    roundness: f32,
) {
    let radius = f64::from(radius);
    let corner_radius = radius * f64::from(roundness);
    let corner_center = radius - corner_radius;
    let control = corner_radius * CIRCLE_KAPPA;
    let mut cap = kurbo::BezPath::new();
    cap.move_to((0.0, -radius));
    cap.line_to((corner_center, -radius));
    cap.curve_to(
        (corner_center + control, -radius),
        (radius, -corner_center - control),
        (radius, -corner_center),
    );
    cap.line_to((radius, corner_center));
    cap.curve_to(
        (radius, corner_center + control),
        (corner_center + control, radius),
        (corner_center, radius),
    );
    cap.line_to((0.0, radius));
    cap.close_path();
    cap.apply_affine(
        kurbo::Affine::rotate(outward[1].atan2(outward[0]))
            .then_translate(kurbo::Vec2::new(center[0], center[1])),
    );
    path.extend(cap);
}

fn kurbo_point(point: Point) -> kurbo::Point {
    kurbo::Point::new(f64::from(point.x), f64::from(point.y))
}

fn push_distinct(points: &mut Vec<Point>, point: Point) {
    if points.last() != Some(&point) {
        points.push(point);
    }
}
