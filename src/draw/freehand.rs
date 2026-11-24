use kurbo::{BezPath, Cap, ParamCurve, ParamCurveArclen, Shape, Stroke, StrokeOpts};
use std::time::Instant;

use super::scene::{
    ElementKind, Point, Style, kurbo_point, pixel_aligned_point, pixel_aligned_points,
};
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
    centerline: kurbo::Point,
    raw_index: usize,
}

#[derive(Debug)]
pub(super) struct LiveStroke {
    points: Vec<Point>,
    started: Option<Instant>,
    timing: Vec<(f32, f32)>,
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
    pub fn new(point: Point, style: Style, temporary: bool) -> Self {
        let aligned = pixel_aligned_point(point, style.size);
        Self {
            points: vec![aligned],
            started: temporary.then(Instant::now),
            timing: if temporary {
                vec![(0.0, 0.0)]
            } else {
                Vec::new()
            },
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

    pub fn update_style(&mut self, update: impl FnOnce(&mut Style)) {
        let mut style = self.style;
        update(&mut style);
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

    pub fn finish(mut self, point: Point, snap: bool) -> (ElementKind, Style, Geometry) {
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
        let duration = self.started.map(|started| started.elapsed().as_secs_f32());
        while self.cache_ready_chunk() {}
        let geometry = self.tail_geometry();
        if let Some(duration) = duration {
            let mut length = 0.0;
            for ((_, distance), pair) in self.timing.iter_mut().skip(1).zip(self.points.windows(2))
            {
                length += (pair[1] - pair[0]).length();
                *distance = length;
            }
            if duration > 0.0 {
                if length > 0.0 {
                    // Keep elapsed seconds and normalize distance once for playback.
                    for (_, distance) in &mut self.timing {
                        *distance /= length;
                    }
                }
                // Preserve a pause before release without retiming the final movement.
                self.timing.push((duration, 1.0));
            } else {
                self.timing.clear();
            }
        }
        (
            ElementKind::Freehand {
                points: self.points,
                timing: self.timing,
            },
            self.style,
            geometry,
        )
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

    pub fn bounds(&self) -> super::scene::Bounds {
        super::scene::Bounds::from_points(self.points.iter().copied())
            .expanded(self.style.size * 0.5)
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
        Geometry::new(path, Fill::NonZero, self.style.color)
    }

    fn tail_centerline(&self, raw_end: usize, complete: bool) -> Vec<kurbo::Point> {
        let Some(anchor) = &self.cache_anchor else {
            return centerline_points(None, &self.points[..raw_end], self.style.size, complete);
        };
        centerline_points(
            Some(anchor.centerline),
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
        if let Some(started) = self.started {
            self.timing.resize(points.len(), (0.0, 0.0));
            if points.len() > 1 {
                self.timing.last_mut().unwrap().0 = started.elapsed().as_secs_f32();
            }
        }
        true
    }
}

pub(super) fn geometry(points: &[Point], style: Style) -> Geometry {
    let path = match snapped_samples(points, style, 1.0) {
        None => return Geometry::default(),
        Some(Err(stamp)) => stamp,
        Some(Ok(points)) => {
            let centerline = centerline_points(None, &points, style.size, true);
            centerline_path(&centerline, style, false, false)
        }
    };
    Geometry::new(path, Fill::NonZero, style.color)
}

pub(super) fn erasing_geometry(
    points: &[Point],
    timing: &[(f32, f32)],
    style: Style,
    remaining: f32,
) -> Geometry {
    let points = match snapped_samples(points, style, remaining) {
        None => return Geometry::default(),
        Some(Err(stamp)) => return Geometry::new(stamp, Fill::NonZero, style.color),
        Some(Ok(points)) => points,
    };
    let centerline = centerline_points(None, &points, style.size, true);
    let segments = smoothed_centerline(&centerline, false, false)
        .segments()
        .map(|segment| (segment, segment.arclen(0.01)))
        .collect::<Vec<_>>();
    let elapsed = (1.0 - remaining) * timing.last().map_or(1.0, |&(duration, _)| duration);
    let index = timing.partition_point(|&(time, _)| time <= elapsed);
    let progress = match (
        index.checked_sub(1).and_then(|i| timing.get(i)),
        timing.get(index),
    ) {
        (Some(&(start_time, start)), Some(&(end_time, end))) => {
            start + (end - start) * ((elapsed - start_time) / (end_time - start_time))
        }
        (Some(&(_, progress)), None) => progress,
        _ => elapsed,
    };
    let mut erased = f64::from(progress) * segments.iter().map(|(_, length)| length).sum::<f64>();
    // Cut the original smoothed curve so the remaining ink does not shift.
    let tail = BezPath::from_path_segments(segments.into_iter().filter_map(|(segment, length)| {
        if erased >= length {
            erased -= length;
            return None;
        }
        let start = segment.inv_arclen(erased, 0.01);
        erased = 0.0;
        Some(segment.subsegment(start..1.0))
    }));
    let (Some(first), Some(last)) = (tail.segments().next(), tail.segments().last()) else {
        return Geometry::default();
    };
    let caps = [(first, true), (last, false)].map(|(segment, start)| {
        let curve = segment.to_cubic();
        let (center, controls) = if start {
            (curve.p0, [curve.p1, curve.p2, curve.p3])
        } else {
            (curve.p3, [curve.p2, curve.p1, curve.p0])
        };
        let outward = controls
            .into_iter()
            .map(|point| center - point)
            .find(|direction| direction.hypot2() > f64::EPSILON)
            .unwrap_or_default();
        (center, outward)
    });
    let path = capped_stroke(&tail, style, caps);
    Geometry::new(path, Fill::NonZero, style.color)
}

// Snaps samples to the pixel grid, or returns a brush stamp scaled by `scale` for a dot.
fn snapped_samples(
    points: &[Point],
    style: Style,
    scale: f32,
) -> Option<Result<Vec<Point>, BezPath>> {
    let mut points = pixel_aligned_points(points, style.size).into_owned();
    points.dedup();
    let (&first, &last) = (points.first()?, points.last()?);
    let radius = style.size * 0.5;
    if radius <= f32::EPSILON {
        return None;
    }
    if points.len() > 2 || first.distance_squared(last) >= MIN_SAMPLE_DISTANCE_SQUARED {
        return Some(Ok(points));
    }
    let direction = last - first;
    let angle = direction.y.atan2(direction.x) + std::f32::consts::FRAC_PI_2;
    let roundness = style.roundness.clamp(0.0, 1.0);
    let stamp = brush_stamp(kurbo_point(first), radius * scale, roundness, angle);
    Some(Err(stamp))
}

fn centerline_points(
    start: Option<kurbo::Point>,
    points: &[Point],
    width: f32,
    complete: bool,
) -> Vec<kurbo::Point> {
    let input = start
        .into_iter()
        .chain(points.iter().copied().map(kurbo_point))
        .map(|point| perfect_freehand::InputPoint::Array([point.x, point.y], None))
        .collect::<Vec<_>>();
    let options = perfect_freehand::StrokeOptions {
        size: Some(f64::from(width)),
        streamline: Some(0.5),
        last: Some(complete),
        ..Default::default()
    };
    perfect_freehand::get_stroke_points(&input, &options)
        .into_iter()
        .map(|point| kurbo::Point::new(point.point[0], point.point[1]))
        .collect()
}

fn centerline_path(
    centerline: &[kurbo::Point],
    style: Style,
    start_cut: bool,
    end_cut: bool,
) -> BezPath {
    let smoothed = smoothed_centerline(centerline, start_cut, end_cut);
    let (&[start, second, ..], &[.., penultimate, end]) = (centerline, centerline) else {
        return capped_stroke(&smoothed, style, []);
    };
    let caps = [
        (!start_cut).then_some((start, start - second)),
        (!end_cut).then_some((end, end - penultimate)),
    ];
    capped_stroke(&smoothed, style, caps.into_iter().flatten())
}

// Strokes the centerline with butt ends and appends a partial cap at each endpoint.
fn capped_stroke(
    centerline: &BezPath,
    style: Style,
    caps: impl IntoIterator<Item = (kurbo::Point, kurbo::Vec2)>,
) -> BezPath {
    let radius = style.size.max(0.0) * 0.5;
    let roundness = style.roundness.clamp(0.0, 1.0);
    let stroke = Stroke::new(f64::from(radius) * 2.0).with_caps(Cap::Butt);
    let mut path = kurbo::stroke(centerline, &stroke, &StrokeOpts::default(), 0.1);
    for (center, outward) in caps {
        append_partial_cap(&mut path, center, outward, radius, roundness);
    }
    path
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
