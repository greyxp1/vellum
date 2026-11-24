use super::scene::{Point, kurbo_point};
use crate::Rgba;
use crate::config::ToolIcon;
use crate::render::{Geometry, LocalGeometry, contrasting_color};
use crate::tool::{ICON_COLOR, Icon};
use kurbo::{Arc, BezPath, Circle, Shape, Vec2};
use peniko::Fill;

const CENTER_RADIUS: f32 = 26.0;
const COLOR_OUTER_RADIUS: f32 = 78.0;
const TOOL_INNER_RADIUS: f32 = 82.0;
const TOOL_OUTER_RADIUS: f32 = 118.0;
const TOOL_ICON_RADIUS: f32 = (TOOL_INNER_RADIUS + TOOL_OUTER_RADIUS) * 0.5;
const PICKER_LAYER_SIZE: u32 = 256;
const WHEEL_BORDER_WIDTH: f32 = 3.0;
const HOVER_EXTENSION: f32 = 6.0;
const PREVIEW_BORDER_WIDTH: f32 = 1.0;
const SEPARATOR_HALF_WIDTH: f32 = 2.0;

const GAP_LINE_COLOR: [f32; 4] = [0.03, 0.03, 0.03, 1.0];
const HOVER_COLOR: [f32; 4] = rgb_to_f32(4, 131, 250, 0.98);
const ACTIVE_COLOR: [f32; 4] = rgb_to_f32(57, 100, 136, 0.95);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    Color(usize),
    Tool(usize),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Picker {
    pub center: Point,
    pub hovered: Option<Choice>,
}

impl Picker {
    pub(super) fn choice(
        self,
        point: Point,
        color_count: usize,
        tool_count: usize,
    ) -> Option<Choice> {
        let distance = (point - self.center).length();
        if distance < CENTER_RADIUS {
            None
        } else if distance < (COLOR_OUTER_RADIUS + TOOL_INNER_RADIUS) * 0.5 {
            Some(Choice::Color(radial_index(self.center, point, color_count)))
        } else {
            Some(Choice::Tool(radial_index(self.center, point, tool_count)))
        }
    }
}

fn radial_index(center: Point, point: Point, count: usize) -> usize {
    let delta = point - center;
    let step = std::f32::consts::TAU / count as f32;
    // Floating-point remainder can round up to TAU at the wrap boundary.
    (((delta.y.atan2(delta.x) + step * 0.5).rem_euclid(std::f32::consts::TAU) / step).floor()
        as usize)
        % count
}

fn push_color_preview(output: &mut Geometry, center: Point, color: Rgba, presentation: bool) {
    let center = kurbo_point(center);
    let radius = CENTER_RADIUS - WHEEL_BORDER_WIDTH - PREVIEW_BORDER_WIDTH;
    let inner = Circle::new(center, f64::from(radius)).to_path(0.02);
    output.push(inner, Fill::NonZero, opaque(color));
    if !presentation {
        return;
    }
    let mut path = Circle::new(center, 8.0).to_path(0.02);
    path.move_to(center + (0.0, -4.0));
    path.line_to(center);
    path.line_to(center + (3.0, 2.0));
    let stroke = kurbo::Stroke::new(1.8)
        .with_caps(kurbo::Cap::Round)
        .with_join(kurbo::Join::Round);
    output.push(path, stroke, contrasting_color(opaque(color)));
}

// Pushes segment `index` of `count` between two radii, insetting its edges by `edge_inset`
// and drawing `border` behind it.
fn push_segment(
    output: &mut Geometry,
    center: Point,
    [inner, outer]: [f32; 2],
    (index, count): (usize, usize),
    edge_inset: f32,
    border: Option<Rgba>,
    color: Rgba,
) {
    let step = std::f32::consts::TAU / count as f32;
    let wedge = |inner: f32, outer: f32, edge_inset: f32| {
        let center = kurbo_point(center);
        let mut path = BezPath::new();
        // The inner arc runs forward and the outer arc returns, joined by straight edges.
        for (radius, direction) in [(inner, 1.0), (outer, -1.0)] {
            let half = (step * 0.5 - (edge_inset / radius).asin()) * direction;
            let start = f64::from(index as f32 * step - half);
            let radius = f64::from(radius);
            let from = center + Vec2::from_angle(start) * radius;
            if path.is_empty() {
                path.move_to(from);
            } else {
                path.line_to(from);
            }
            let arc = Arc::new(center, (radius, radius), start, f64::from(2.0 * half), 0.0);
            path.extend(arc.append_iter(0.02));
        }
        path.close_path();
        path
    };
    if let Some(border) = border {
        let path = wedge(inner - WHEEL_BORDER_WIDTH, outer + WHEEL_BORDER_WIDTH, 0.0);
        output.push(path, Fill::NonZero, border);
    }
    output.push(wedge(inner, outer, edge_inset), Fill::NonZero, color);
}

pub(super) fn picker_bounds(center: Point) -> kurbo::Rect {
    kurbo::Rect::from_origin_size(
        (
            f64::from((center.x - PICKER_LAYER_SIZE as f32 * 0.5).floor()),
            f64::from((center.y - PICKER_LAYER_SIZE as f32 * 0.5).floor()),
        ),
        (f64::from(PICKER_LAYER_SIZE), f64::from(PICKER_LAYER_SIZE)),
    )
}

pub(super) fn picker_geometry<'a>(
    picker: Picker,
    active: Option<usize>,
    current_color: Rgba,
    tools: &[(&'a ToolIcon, bool)],
    palette: &[Rgba],
    viewport: kurbo::Rect,
    presentation: bool,
) -> Option<LocalGeometry<'a>> {
    let Picker { center, hovered } = picker;
    let bounds = picker_bounds(center);
    let origin = [bounds.x0 as f32, bounds.y0 as f32];
    if bounds.intersect(viewport.inflate(1.0, 1.0)).area() == 0.0 {
        return None;
    }
    let center = Point::new(center.x - origin[0], center.y - origin[1]);
    let radius = f64::from(TOOL_OUTER_RADIUS + WHEEL_BORDER_WIDTH);
    let backdrop = Circle::new(kurbo_point(center), radius).to_path(0.02);
    let mut output = Geometry::new(backdrop, Fill::NonZero, GAP_LINE_COLOR);
    push_color_preview(&mut output, center, current_color, presentation);
    let mut svg_icons = Vec::new();
    for (index, &(tool, filled)) in tools.iter().enumerate() {
        let is_hovered = hovered == Some(Choice::Tool(index));
        let outer = TOOL_OUTER_RADIUS + if is_hovered { HOVER_EXTENSION } else { 0.0 };
        let color = if is_hovered {
            HOVER_COLOR
        } else if Some(index) == active {
            ACTIVE_COLOR
        } else {
            [0.16, 0.18, 0.22, 0.95]
        };
        let border = is_hovered.then_some(GAP_LINE_COLOR);
        let segment = (index, tools.len());
        let radii = [TOOL_INNER_RADIUS, outer];
        let inset = SEPARATOR_HALF_WIDTH;
        push_segment(&mut output, center, radii, segment, inset, border, color);
        // Wedges stay within their segment, so each icon can follow its own wedge.
        let angle = index as f32 * std::f32::consts::TAU / tools.len() as f32;
        let icon_center = center + Point::new(angle.cos(), angle.sin()) * TOOL_ICON_RADIUS;
        match tool {
            ToolIcon::Named(icon) => push_tool_icon(&mut output, icon_center, *icon, filled),
            ToolIcon::Svg(svg) => svg_icons.push((svg, kurbo_point(icon_center).to_vec2())),
        }
    }
    for (index, &color) in palette.iter().enumerate() {
        let is_hovered = hovered == Some(Choice::Color(index));
        let inset = if is_hovered {
            WHEEL_BORDER_WIDTH
        } else {
            SEPARATOR_HALF_WIDTH
        };
        let border = is_hovered.then_some(HOVER_COLOR);
        let segment = (index, palette.len());
        let radii = [CENTER_RADIUS, COLOR_OUTER_RADIUS];
        let color = opaque(color);
        push_segment(&mut output, center, radii, segment, inset, border, color);
    }
    Some(LocalGeometry::new(
        output,
        svg_icons,
        origin,
        [PICKER_LAYER_SIZE; 2],
    ))
}

const fn rgb_to_f32(r: u8, g: u8, b: u8, alpha: f32) -> [f32; 4] {
    [r as f32 / 255., g as f32 / 255., b as f32 / 255., alpha]
}

fn opaque([red, green, blue, _]: [f32; 4]) -> [f32; 4] {
    [red, green, blue, 1.0]
}

fn push_tool_icon(output: &mut Geometry, center: Point, tool: Icon, filled: bool) {
    const SCALE: f32 = 0.82;
    let p = |(x, y): (f32, f32)| {
        (
            f64::from(center.x + (x - 12.0) * SCALE),
            f64::from(center.y + (y - 12.0) * SCALE),
        )
    };
    let mut path = BezPath::new();
    // Each subpath is an SVG-style polyline that closes into a polygon when flagged.
    let subpaths: &[(bool, &[(f32, f32)])] = match tool {
        Icon::Pen => &[
            (false, &[(13.0, 21.0), (21.0, 21.0)]),
            (
                true,
                &[
                    (3.8, 16.2),
                    (17.2, 2.8),
                    (21.2, 6.8),
                    (7.8, 20.2),
                    (2.5, 21.5),
                ],
            ),
        ],
        Icon::Marker => &[
            (true, &[(3.0, 16.0), (14.0, 5.0), (19.0, 10.0), (8.0, 21.0)]),
            (false, &[(6.0, 13.0), (11.0, 18.0)]),
            (false, &[(2.0, 21.0), (5.0, 18.0)]),
        ],
        Icon::Line => &[(false, &[(5.0, 12.0), (19.0, 12.0)])],
        Icon::Arrow => &[
            (false, &[(7.0, 7.0), (17.0, 7.0), (17.0, 17.0)]),
            (false, &[(7.0, 17.0), (17.0, 7.0)]),
        ],
        Icon::Triangle => &[(true, &[(12.0, 3.0), (21.0, 20.0), (3.0, 20.0)])],
        Icon::Rectangle => &[(
            true,
            &[
                (5.0, 3.0),
                (19.0, 3.0),
                (21.0, 5.0),
                (21.0, 19.0),
                (19.0, 21.0),
                (5.0, 21.0),
                (3.0, 19.0),
                (3.0, 5.0),
            ],
        )],
        Icon::Ellipse => {
            path = Circle::new(p((12.0, 12.0)), f64::from(10.0 * SCALE)).to_path(0.02);
            &[]
        }
        Icon::Text => &[
            (false, &[(12.0, 4.0), (12.0, 20.0)]),
            (false, &[(4.0, 7.0), (4.0, 4.0), (20.0, 4.0), (20.0, 7.0)]),
            (false, &[(9.0, 20.0), (15.0, 20.0)]),
        ],
        Icon::Select => &[(
            true,
            &[(4.0, 4.7), (20.7, 11.0), (14.5, 13.0), (12.0, 20.7)],
        )],
        Icon::Eraser => &[
            (
                true,
                &[
                    (3.0, 15.0),
                    (14.0, 4.0),
                    (21.0, 11.0),
                    (12.0, 20.0),
                    (8.0, 20.0),
                ],
            ),
            (false, &[(8.0, 10.0), (15.0, 17.0)]),
        ],
    };
    for &(closed, points) in subpaths {
        let [start, rest @ ..] = points else { continue };
        path.move_to(p(*start));
        for &point in rest {
            path.line_to(p(point));
        }
        if closed {
            path.close_path();
        }
    }
    if filled {
        output.push(path.clone(), Fill::NonZero, ICON_COLOR);
    }
    output.push(path, kurbo::Stroke::new(2.0), ICON_COLOR);
}
