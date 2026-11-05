//! Annotation data, cached geometry and hit testing.

mod shapes;

use shapes::pixel_aligned_segment;
pub(super) use shapes::{
    append_rounded_contour, bounds_for, geometry, pixel_aligned_point, pixel_aligned_points,
    rendered_segment_endpoints, rounded_corner, rounded_tip, text_bounds,
};

use crate::render::Geometry;
use peniko::Fill;
use std::borrow::Cow;
use std::time::Instant;

use crate::config::{ErasePace, PenEffect, PresentationSettings};

const HIT_SLOP: f32 = 5.0;

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

    pub(crate) fn distance_squared(self, other: Self) -> f32 {
        (self.x - other.x).powi(2) + (self.y - other.y).powi(2)
    }

    pub(super) fn length(self) -> f32 {
        self.x.hypot(self.y)
    }

    pub(super) fn midpoint(self, other: Self) -> Self {
        (self + other) * 0.5
    }

    pub(crate) fn segment_distance_squared(self, start: Self, end: Self) -> f32 {
        let delta = end - start;
        let length_squared = delta.distance_squared(Self::default());
        if length_squared <= f32::EPSILON {
            return self.distance_squared(start);
        }
        let offset = self - start;
        let fraction = ((offset.x * delta.x + offset.y * delta.y) / length_squared).clamp(0.0, 1.0);
        self.distance_squared(start + delta * fraction)
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
    pub(super) fn from_points(points: impl IntoIterator<Item = Point>) -> Self {
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

    pub(super) fn intersects(self, other: Self) -> bool {
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub size: f32,
    pub color: [f32; 4],
    pub roundness: f32,
    pub filled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ElementKind {
    Freehand {
        points: Vec<Point>,
        timing: Vec<(f32, f32)>,
    },
    Segment {
        points: [Point; 2],
        arrow: bool,
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
        text: crate::text::Text,
        scale: [f32; 2],
    },
}

impl ElementKind {
    pub(in crate::draw) fn prepare(&mut self, style: Style) {
        if let Self::Text { text, .. } = self {
            text.set_size(style.size);
        }
    }
    pub(in crate::draw) fn ellipse(min: Point, max: Point) -> Self {
        Self::Ellipse {
            center: min.midpoint(max),
            radii: (max - min) * 0.5,
        }
    }

    pub(super) fn translated(&self, delta: Point) -> Self {
        let mut translated = self.clone();
        match &mut translated {
            Self::Freehand { points, .. } => {
                points.iter_mut().for_each(|point| *point = *point + delta);
            }
            Self::Segment { points, .. } => {
                points.iter_mut().for_each(|point| *point = *point + delta);
            }
            Self::Triangle { vertices } => vertices
                .iter_mut()
                .for_each(|point| *point = *point + delta),
            Self::Rectangle { min, max } => {
                *min = *min + delta;
                *max = *max + delta;
            }
            Self::Ellipse { center, .. } => *center = *center + delta,
            Self::Text { origin, .. } => *origin = *origin + delta,
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
    pub fade_started_at: Option<std::time::Instant>,
}

impl Element {
    pub(super) fn remaining(
        &self,
        now: Instant,
        settings: PresentationSettings,
        protected: bool,
    ) -> f32 {
        let Some(started) = self.fade_started_at else {
            return 1.0;
        };
        if protected {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(started);
        let duration = match &self.kind {
            ElementKind::Freehand { timing, .. } if settings.pen_effect == PenEffect::Erase => {
                match (settings.erase_duration, timing.last()) {
                    (Some(duration), _) => duration.as_secs_f32(),
                    (None, Some(&(duration, _))) => duration,
                    _ => settings.fade_duration.as_secs_f32(),
                }
            }
            _ => settings.fade_duration.as_secs_f32(),
        };
        1.0 - (elapsed.saturating_sub(settings.hold).as_secs_f32() / duration).min(1.0)
    }

    pub(super) fn presentation_geometry(
        &self,
        remaining: f32,
        settings: PresentationSettings,
    ) -> (Cow<'_, Geometry>, f32) {
        if let ElementKind::Freehand { points, timing } = &self.kind
            && settings.pen_effect == PenEffect::Erase
            && remaining < 1.0
        {
            let timing = if settings.erase_pace == ErasePace::Recorded {
                timing.as_slice()
            } else {
                &[]
            };
            return (
                Cow::Owned(super::freehand::erasing_geometry(
                    points, timing, self.style, remaining,
                )),
                1.0,
            );
        }
        (Cow::Borrowed(&self.geometry), remaining)
    }

    pub(super) fn new(id: ElementId, kind: ElementKind, style: Style) -> Self {
        let geometry = geometry(&kind, style);
        Self::with_geometry(id, kind, style, geometry)
    }

    pub(super) fn with_geometry(
        id: ElementId,
        mut kind: ElementKind,
        style: Style,
        geometry: Geometry,
    ) -> Self {
        kind.prepare(style);
        let bounds = bounds_for(&kind, style);
        Self {
            id,
            kind,
            style,
            bounds,
            geometry,
            fade_started_at: None,
        }
    }

    pub(super) fn replace(&mut self, mut kind: ElementKind, style: Style) -> (ElementKind, Style) {
        self.restart_fade();
        kind.prepare(style);
        let same_shape = self.kind == kind
            && self.style.size == style.size
            && self.style.roundness == style.roundness
            && self.style.filled == style.filled;
        let kind = std::mem::replace(&mut self.kind, kind);
        let style = std::mem::replace(&mut self.style, style);
        if same_shape {
            self.geometry.set_color(self.style.color);
        } else {
            self.bounds = bounds_for(&self.kind, self.style);
            self.geometry = geometry(&self.kind, self.style);
        }
        (kind, style)
    }

    pub(super) fn restart_fade(&mut self) {
        self.fade_started_at = self.fade_started_at.map(|_| std::time::Instant::now());
    }

    pub(super) fn erase_hit_test(
        &self,
        start: Point,
        end: Point,
        radius: f32,
        remaining: f32,
        settings: PresentationSettings,
    ) -> bool {
        use kurbo::Shape;
        let swept_bounds = Bounds::from_points([start, end]).expanded(radius);
        if remaining <= 0.0 || !self.bounds.intersects(swept_bounds) {
            return false;
        }
        let line = kurbo::Line::new(kurbo_point(start), kurbo_point(end));
        if matches!(self.kind, ElementKind::Text { .. }) {
            let path = kurbo::Rect::new(
                f64::from(self.bounds.min.x),
                f64::from(self.bounds.min.y),
                f64::from(self.bounds.max.x),
                f64::from(self.bounds.max.y),
            )
            .to_path(0.1);
            return Geometry::fill(path, Fill::NonZero, self.style.color)
                .swept_hit_test(line, f64::from(radius));
        }
        self.presentation_geometry(remaining, settings)
            .0
            .swept_hit_test(line, f64::from(radius))
    }

    pub(super) fn hit_test(
        &self,
        point: Point,
        remaining: f32,
        settings: PresentationSettings,
    ) -> bool {
        if remaining <= 0.0 || !self.bounds.expanded(HIT_SLOP).contains(point) {
            return false;
        }
        let tolerance = self.style.size * 0.5 + HIT_SLOP;
        match &self.kind {
            ElementKind::Segment {
                points: [start, end],
                arrow: false,
            } => {
                let (start, end) = pixel_aligned_segment(*start, *end, self.style.size);
                point.segment_distance_squared(start, end) <= tolerance * tolerance
            }
            ElementKind::Freehand { .. }
            | ElementKind::Segment { arrow: true, .. }
            | ElementKind::Rectangle { .. }
            | ElementKind::Triangle { .. } => self
                .presentation_geometry(remaining, settings)
                .0
                .swept_hit_test(
                    kurbo::Line::new(kurbo_point(point), kurbo_point(point)),
                    f64::from(HIT_SLOP),
                ),
            ElementKind::Ellipse { center, radii } => {
                let local = point - *center;
                (self.style.filled
                    && radii.x > 0.0
                    && radii.y > 0.0
                    && (local.x / radii.x).powi(2) + (local.y / radii.y).powi(2) <= 1.0)
                    || ellipse_distance(local, *radii) <= tolerance
            }
            ElementKind::Text { .. } => self.bounds.contains(point),
        }
    }
}

fn ellipse_distance(point: Point, radii: Point) -> f32 {
    let (x, y, a, b) = if radii.x >= radii.y {
        (point.x.abs(), point.y.abs(), radii.x, radii.y)
    } else {
        (point.y.abs(), point.x.abs(), radii.y, radii.x)
    };
    if b <= f32::EPSILON {
        return (x - a).max(0.0).hypot(y);
    }
    let (x, y, a, b) = (f64::from(x), f64::from(y), f64::from(a), f64::from(b));
    let (a2, b2) = (a * a, b * b);
    let (nearest_x, nearest_y) = if y == 0.0 {
        let difference = a2 - b2;
        if a * x < difference {
            let nearest_x = a2 * x / difference;
            (nearest_x, b * (1.0 - (nearest_x / a).powi(2)).sqrt())
        } else {
            (a, 0.0)
        }
    } else if x == 0.0 {
        (0.0, b)
    } else {
        // The nearest point satisfies the ellipse equation with one scalar multiplier.
        // Its residual decreases monotonically above -b², so bisection stays bounded.
        let mut low = b * (y - b);
        let mut high = (a * x + b * y).max(0.0);
        for _ in 0..64 {
            let t = (low + high) * 0.5;
            let residual = (a * x / (t + a2)).powi(2) + (b * y / (t + b2)).powi(2);
            if residual > 1.0 {
                low = t;
            } else {
                high = t;
            }
        }
        let t = (low + high) * 0.5;
        (a2 * x / (t + a2), b2 * y / (t + b2))
    };
    (x - nearest_x).hypot(y - nearest_y) as f32
}

pub(super) fn kurbo_point(point: Point) -> kurbo::Point {
    kurbo::Point::new(f64::from(point.x), f64::from(point.y))
}

pub(super) fn tool_for(kind: &ElementKind) -> crate::tool::Tool {
    use crate::tool::Tool;

    match kind {
        ElementKind::Freehand { .. } => Tool::Pen,
        ElementKind::Segment { arrow: true, .. } => Tool::Arrow,
        ElementKind::Segment { arrow: false, .. } => Tool::Line,
        ElementKind::Triangle { .. } => Tool::Triangle,
        ElementKind::Rectangle { .. } => Tool::Rectangle,
        ElementKind::Ellipse { .. } => Tool::Ellipse,
        ElementKind::Text { .. } => Tool::Text,
    }
}
