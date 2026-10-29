use kurbo::{BezPath, Cap, Shape, Stroke, StrokeOpts};

use super::scene::{Point, Style, kurbo_point, pixel_aligned_point, pixel_aligned_points};
use super::{CIRCLE_KAPPA, PenMotion};
use crate::render::Geometry;
use peniko::Fill;

const MIN_SAMPLE_DISTANCE_SQUARED: f32 = 1.0;
const STAMP_DIRECTION_LENGTH: f32 = 0.01;
const SNAP_ANGLE: f32 = std::f32::consts::PI / 12.0;
const CHUNK_POINTS: usize = 256;
const STABILIZER_FOLLOW: f32 = 0.35;

fn stabilizer_delay(size: f32) -> f32 {
    (size * 0.15).clamp(4.0, 16.0)
}

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
    cached: BezPath,
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
            cached: BezPath::new(),
        }
    }

    pub fn push_motion(&mut self, motion: PenMotion, snap: bool) -> bool {
        let Some(bend) = motion.bend else {
            return self.push_with_follow(motion.end, snap, STABILIZER_FOLLOW);
        };
        let follow = 1.0 - (1.0 - STABILIZER_FOLLOW).sqrt();
        let first_changed = self.push_with_follow(bend, snap, follow);
        self.push_with_follow(
            motion.end,
            snap,
            if first_changed {
                follow
            } else {
                STABILIZER_FOLLOW
            },
        ) | first_changed
    }

    fn push_with_follow(&mut self, point: Point, snap: bool, follow: f32) -> bool {
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
        self.stabilized_point = self.stabilized_point + (target - self.stabilized_point) * follow;
        let changed = self.push(self.stabilized_point);
        if changed {
            self.cache_ready_chunk();
        }
        changed
    }

    pub fn release_tip(&self, point: Point, snap: bool) -> Point {
        if self.direction_locked {
            *self.points.last().expect("freehand starts with one point")
        } else {
            self.oriented_point(point + self.alignment_offset, snap)
        }
    }

    pub fn update_style(&mut self, style: Style) {
        if self.style == style {
            return;
        }
        let size_changed = self.style.size != style.size;
        let outline_changed = size_changed || self.style.roundness != style.roundness;
        if size_changed {
            let raw_start = self.points[0] - self.alignment_offset;
            let offset = pixel_aligned_point(raw_start, style.size) - self.points[0];
            self.points
                .iter_mut()
                .for_each(|point| *point = *point + offset);
            self.sample_anchor = self.sample_anchor + offset;
            self.stabilized_point = self.stabilized_point + offset;
            self.alignment_offset = self.alignment_offset + offset;
        }
        self.style = style;
        if outline_changed {
            self.cache_anchor = None;
            self.cached = BezPath::new();
            while self.cache_ready_chunk() {}
        }
    }

    pub fn finish(mut self, point: Point, snap: bool) -> (Vec<Point>, Style, Geometry) {
        if !self.direction_locked {
            let point = self.oriented_point(point + self.alignment_offset, snap);
            if point != self.points[0] {
                self.commit_direction(point);
            }
        }
        if self.points.len() == 1 {
            self.points
                .push(self.points[0] + Point::new(0.0, -STAMP_DIRECTION_LENGTH));
        }
        while self.cache_ready_chunk() {}
        let geometry = self.tail_geometry();
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
        self.push(point);
        self.sample_anchor = point;
        self.sample_pending = false;
        self.stabilized_point = point;
        self.direction_locked = true;
    }

    fn cache_ready_chunk(&mut self) -> bool {
        let raw_start = self
            .cache_anchor
            .as_ref()
            .map_or(0, |anchor| anchor.raw_index);
        if self.points.len() - raw_start <= CHUNK_POINTS + 2 {
            return false;
        }
        let mut raw_end = (raw_start + CHUNK_POINTS * 2).min(self.points.len());
        let centerline = loop {
            let centerline = self.tail_centerline(raw_end, false);
            if centerline.len() > CHUNK_POINTS + 2 {
                break centerline;
            }
            if raw_end == self.points.len() {
                return false;
            }
            raw_end = (raw_end + CHUNK_POINTS).min(self.points.len());
        };

        let split = CHUNK_POINTS;
        self.cached.extend(centerline_path(
            &centerline[..=split + 1],
            self.style,
            self.cache_anchor.is_some(),
            true,
        ));
        let raw_index = self.cache_anchor.as_ref().map_or_else(
            || split + raw_end - centerline.len(),
            |anchor| anchor.raw_index + split,
        );
        self.cache_anchor = Some(CacheAnchor {
            centerline: centerline[split],
            raw_index,
        });
        true
    }

    pub fn tail_geometry(&self) -> Geometry {
        if self.cache_anchor.is_none() {
            return geometry(&self.points, self.style);
        }
        let mut path = self.cached.clone();
        path.extend(centerline_path(
            &self.tail_centerline(self.points.len(), true),
            self.style,
            true,
            false,
        ));
        Geometry::fill(path, Fill::NonZero, self.style.color)
    }

    fn tail_centerline(&self, raw_end: usize, complete: bool) -> Vec<[f64; 2]> {
        let Some(anchor) = &self.cache_anchor else {
            return centerline_points(&[], &self.points[..raw_end], self.style.size, complete);
        };
        centerline_points(
            &[anchor.centerline],
            &self.points[anchor.raw_index + 1..raw_end],
            0.0,
            complete,
        )
    }

    fn push(&mut self, next: Point) -> bool {
        let points = &mut self.points;
        if points.last() == Some(&next) {
            return false;
        }

        if self.sample_anchor.distance_squared(next) >= MIN_SAMPLE_DISTANCE_SQUARED {
            if self.sample_pending {
                *points.last_mut().expect("freehand starts with one point") = next;
            } else {
                points.push(next);
            }
            self.sample_anchor = next;
            self.sample_pending = false;
        } else if self.sample_pending {
            if points.get(points.len().saturating_sub(2)) == Some(&next) {
                points.pop();
                self.sample_pending = false;
            } else {
                *points.last_mut().expect("freehand starts with one point") = next;
            }
        } else {
            points.push(next);
            self.sample_pending = true;
        }
        true
    }
}

pub(super) fn geometry(points: &[Point], style: Style) -> Geometry {
    stroke_path(points, style).map_or_else(Geometry::default, |path| {
        Geometry::fill(path, Fill::NonZero, style.color)
    })
}

fn stroke_path(points: &[Point], style: Style) -> Option<kurbo::BezPath> {
    let points = pixel_aligned_points(points, style.size);
    let points = points.as_ref();
    let distinct = points.windows(2).any(|pair| pair[0] == pair[1]).then(|| {
        let mut distinct = points.to_vec();
        distinct.dedup();
        distinct
    });
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
        &centerline_points(&[], points, style.size, true),
        style,
        false,
        false,
    ))
}

fn centerline_points(
    start: &[[f64; 2]],
    points: &[Point],
    width: f32,
    complete: bool,
) -> Vec<[f64; 2]> {
    let input = start
        .iter()
        .copied()
        .chain(
            points
                .iter()
                .map(|point| [f64::from(point.x), f64::from(point.y)]),
        )
        .map(|point| perfect_freehand::InputPoint::Array(point, None))
        .collect::<Vec<_>>();
    perfect_freehand::get_stroke_points(&input, &stroke_options(width, complete))
        .into_iter()
        .map(|point| point.point)
        .collect()
}

fn stroke_options(width: f32, complete: bool) -> perfect_freehand::StrokeOptions {
    perfect_freehand::StrokeOptions {
        size: Some(f64::from(width)),
        streamline: Some(0.5),
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
    let centerline = centerline_points
        .iter()
        .map(|&[x, y]| kurbo::Point::new(x, y))
        .collect::<Vec<_>>();
    let radius = style.size.max(0.0) * 0.5;
    let roundness = style.roundness.clamp(0.0, 1.0);
    let centerline_path = smoothed_centerline(&centerline, start_cut, end_cut);
    let stroke = Stroke::new(f64::from(radius) * 2.0).with_caps(Cap::Butt);
    let mut path = kurbo::stroke(centerline_path, &stroke, &StrokeOpts::default(), 0.1);
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
    let (&[start, second, ..], &[.., penultimate, end]) = (centerline, centerline) else {
        return;
    };
    if !start_cut {
        append_partial_cap(path, start, start - second, radius, roundness);
    }
    if !end_cut {
        append_partial_cap(path, end, end - penultimate, radius, roundness);
    }
}

fn smoothed_centerline(points: &[kurbo::Point], start_cut: bool, end_cut: bool) -> kurbo::BezPath {
    let mut path = kurbo::BezPath::new();
    let Some((&first, rest)) = points.split_first() else {
        return path;
    };
    let Some(&last) = rest.last() else {
        path.move_to(first);
        return path;
    };

    path.move_to(if start_cut {
        first.midpoint(rest[0])
    } else {
        first
    });
    for pair in rest.windows(2) {
        path.quad_to(pair[0], pair[0].midpoint(pair[1]));
    }
    if !end_cut {
        path.line_to(last);
    }
    path
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
    center: kurbo::Point,
    outward: kurbo::Vec2,
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
    cap.apply_affine(kurbo::Affine::rotate(outward.atan2()).then_translate(center.to_vec2()));
    path.extend(cap);
}
