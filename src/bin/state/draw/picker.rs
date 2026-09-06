use std::ops::Range;

use super::scene::Point;
use super::tool::Tool;
use crate::render::{FillRule, Geometry, LocalGeometry, StrokeStyle};
use kurbo::{Arc, BezPath, Circle, Shape};

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
    Tool(Tool),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Picker {
    pub center: Point,
    pub hovered: Option<Choice>,
}

pub(super) struct ShapeFills {
    pub triangle: bool,
    pub rectangle: bool,
    pub ellipse: bool,
}

impl ShapeFills {
    fn for_tool(&self, tool: Tool) -> bool {
        match tool {
            Tool::Triangle => self.triangle,
            Tool::Rectangle => self.rectangle,
            Tool::Ellipse => self.ellipse,
            _ => false,
        }
    }
}

const TOOL_CHOICES: [Tool; 8] = [
    Tool::Pen,
    Tool::Line,
    Tool::Arrow,
    Tool::Triangle,
    Tool::Rectangle,
    Tool::Ellipse,
    Tool::Select,
    Tool::Text,
];

pub(super) fn choice(center: Point, point: Point, color_count: usize) -> Option<Choice> {
    let distance = (point - center).length();
    if distance < CENTER_RADIUS {
        None
    } else if distance < (COLOR_OUTER_RADIUS + TOOL_INNER_RADIUS) * 0.5 {
        Some(Choice::Color(radial_index(center, point, color_count)))
    } else {
        Some(Choice::Tool(
            TOOL_CHOICES[radial_index(center, point, TOOL_CHOICES.len())],
        ))
    }
}

fn radial_index(center: Point, point: Point, count: usize) -> usize {
    let delta = point - center;
    let step = std::f32::consts::TAU / count as f32;
    ((delta.y.atan2(delta.x) + step * 0.5).rem_euclid(std::f32::consts::TAU) / step).floor()
        as usize
}

fn radial_point(center: Point, radius: f32, angle: f32) -> kurbo::Point {
    let (sin, cos) = angle.sin_cos();
    kurbo::Point::new(
        f64::from(center.x + radius * cos),
        f64::from(center.y + radius * sin),
    )
}

fn push_disc(output: &mut Geometry, center: Point, radius: f32, color: [f32; 4]) {
    output.append(Geometry::fill(
        Circle::new(
            (f64::from(center.x), f64::from(center.y)),
            f64::from(radius),
        )
        .to_path(0.02),
        FillRule::NonZero,
        color,
    ));
}

fn push_color_preview(output: &mut Geometry, center: Point, color: [f32; 4]) {
    let preview_radius = CENTER_RADIUS - WHEEL_BORDER_WIDTH;
    push_disc(output, center, CENTER_RADIUS, GAP_LINE_COLOR);
    push_disc(
        output,
        center,
        preview_radius - PREVIEW_BORDER_WIDTH,
        opaque(color),
    );
}

fn push_wedge(
    output: &mut Geometry,
    center: Point,
    inner: f32,
    outer: f32,
    angles: Range<f32>,
    edge_inset: f32,
    color: [f32; 4],
) {
    let inner_inset = (edge_inset / inner).asin();
    let outer_inset = (edge_inset / outer).asin();
    let inner_start = angles.start + inner_inset;
    let inner_end = angles.end - inner_inset;
    let outer_start = angles.start + outer_inset;
    let outer_end = angles.end - outer_inset;
    let center = (f64::from(center.x), f64::from(center.y));
    let mut path = BezPath::new();
    path.move_to(radial_point(
        Point::new(center.0 as f32, center.1 as f32),
        inner,
        inner_start,
    ));
    path.extend(
        Arc::new(
            center,
            (f64::from(inner), f64::from(inner)),
            f64::from(inner_start),
            f64::from(inner_end - inner_start),
            0.0,
        )
        .append_iter(0.02),
    );
    path.line_to(radial_point(
        Point::new(center.0 as f32, center.1 as f32),
        outer,
        outer_end,
    ));
    path.extend(
        Arc::new(
            center,
            (f64::from(outer), f64::from(outer)),
            f64::from(outer_end),
            f64::from(outer_start - outer_end),
            0.0,
        )
        .append_iter(0.02),
    );
    path.close_path();
    output.append(Geometry::fill(path, FillRule::NonZero, color));
}

fn push_palette(
    output: &mut Geometry,
    center: Point,
    hovered: Option<Choice>,
    palette: &[[f32; 4]],
) {
    let step = std::f32::consts::TAU / palette.len() as f32;
    for (index, &color) in palette.iter().enumerate() {
        let is_hovered = hovered == Some(Choice::Color(index));
        let start = index as f32 * step - step * 0.5;
        let end = start + step;
        push_wedge(
            output,
            center,
            CENTER_RADIUS - WHEEL_BORDER_WIDTH,
            COLOR_OUTER_RADIUS + WHEEL_BORDER_WIDTH,
            start..end,
            0.0,
            if is_hovered {
                HOVER_COLOR
            } else {
                GAP_LINE_COLOR
            },
        );
        push_wedge(
            output,
            center,
            CENTER_RADIUS,
            COLOR_OUTER_RADIUS,
            start..end,
            if is_hovered {
                WHEEL_BORDER_WIDTH
            } else {
                SEPARATOR_HALF_WIDTH
            },
            opaque(color),
        );
    }
}

pub(super) fn picker_geometry(
    center: Point,
    hovered: Option<Choice>,
    active: Tool,
    current_color: [f32; 4],
    tool_fills: ShapeFills,
    palette: &[[f32; 4]],
) -> LocalGeometry {
    let mut output = Geometry::empty();
    let origin = [
        (center.x - PICKER_LAYER_SIZE as f32 * 0.5).floor(),
        (center.y - PICKER_LAYER_SIZE as f32 * 0.5).floor(),
    ];
    let local_center = Point::new(center.x - origin[0], center.y - origin[1]);
    push_color_preview(&mut output, local_center, current_color);
    let step = std::f32::consts::TAU / TOOL_CHOICES.len() as f32;
    for (index, tool) in TOOL_CHOICES.iter().copied().enumerate() {
        let is_hovered = hovered == Some(Choice::Tool(tool));
        let is_active = tool == active;
        let outer = TOOL_OUTER_RADIUS + if is_hovered { HOVER_EXTENSION } else { 0.0 };
        let start = index as f32 * step - step * 0.5;
        let end = start + step;
        push_wedge(
            &mut output,
            local_center,
            TOOL_INNER_RADIUS - WHEEL_BORDER_WIDTH,
            outer + WHEEL_BORDER_WIDTH,
            start..end,
            0.0,
            GAP_LINE_COLOR,
        );
        push_wedge(
            &mut output,
            local_center,
            TOOL_INNER_RADIUS,
            outer,
            start..end,
            SEPARATOR_HALF_WIDTH,
            if is_hovered {
                HOVER_COLOR
            } else if is_active {
                ACTIVE_COLOR
            } else {
                [0.16, 0.18, 0.22, 0.95]
            },
        );
    }
    push_palette(&mut output, local_center, hovered, palette);
    for (index, tool) in TOOL_CHOICES.iter().copied().enumerate() {
        let (sin, cos) = (index as f32 * step).sin_cos();
        let tool_icon_center = Point::new(
            local_center.x + TOOL_ICON_RADIUS * cos,
            local_center.y + TOOL_ICON_RADIUS * sin,
        );
        let filled = tool_fills.for_tool(tool);
        push_tool_icon(&mut output, tool_icon_center, tool, filled);
    }
    LocalGeometry::new(output, origin, [PICKER_LAYER_SIZE; 2])
}

const fn rgb_to_f32(r: u8, g: u8, b: u8, alpha: f32) -> [f32; 4] {
    [r as f32 / 255., g as f32 / 255., b as f32 / 255., alpha]
}

fn opaque([red, green, blue, _]: [f32; 4]) -> [f32; 4] {
    [red, green, blue, 1.0]
}

fn push_tool_icon(output: &mut Geometry, center: Point, tool: Tool, filled: bool) {
    const SCALE: f32 = 0.82;
    let p = |x: f32, y: f32| {
        (
            f64::from(center.x + (x - 12.0) * SCALE),
            f64::from(center.y + (y - 12.0) * SCALE),
        )
    };
    let mut path = BezPath::new();
    match tool {
        Tool::Pen => {
            path.move_to(p(13.0, 21.0));
            path.line_to(p(21.0, 21.0));
            path.move_to(p(3.8, 16.2));
            path.line_to(p(17.2, 2.8));
            path.line_to(p(21.2, 6.8));
            path.line_to(p(7.8, 20.2));
            path.line_to(p(2.5, 21.5));
            path.close_path();
        }
        Tool::Line => {
            path.move_to(p(5.0, 12.0));
            path.line_to(p(19.0, 12.0));
        }
        Tool::Arrow => {
            path.move_to(p(7.0, 7.0));
            path.line_to(p(17.0, 7.0));
            path.line_to(p(17.0, 17.0));
            path.move_to(p(7.0, 17.0));
            path.line_to(p(17.0, 7.0));
        }
        Tool::Triangle => {
            path.move_to(p(12.0, 3.0));
            path.line_to(p(21.0, 20.0));
            path.line_to(p(3.0, 20.0));
            path.close_path();
        }
        Tool::Rectangle => {
            path.move_to(p(5.0, 3.0));
            path.line_to(p(19.0, 3.0));
            path.line_to(p(21.0, 5.0));
            path.line_to(p(21.0, 19.0));
            path.line_to(p(19.0, 21.0));
            path.line_to(p(5.0, 21.0));
            path.line_to(p(3.0, 19.0));
            path.line_to(p(3.0, 5.0));
            path.close_path();
        }
        Tool::Ellipse => {
            path = Circle::new(p(12.0, 12.0), f64::from(10.0 * SCALE)).to_path(0.02);
        }
        Tool::Text => {
            path.move_to(p(12.0, 4.0));
            path.line_to(p(12.0, 20.0));
            path.move_to(p(4.0, 7.0));
            path.line_to(p(4.0, 4.0));
            path.line_to(p(20.0, 4.0));
            path.line_to(p(20.0, 7.0));
            path.move_to(p(9.0, 20.0));
            path.line_to(p(15.0, 20.0));
        }
        Tool::Select => {
            path.move_to(p(4.0, 4.7));
            path.line_to(p(20.7, 11.0));
            path.line_to(p(14.5, 13.0));
            path.line_to(p(12.0, 20.7));
            path.close_path();
        }
        Tool::Eraser => unreachable!("eraser is not a radial choice"),
    }
    let icon_color = [0.96, 0.97, 1.0, 1.0];
    if filled {
        output.append(Geometry::fill(path.clone(), FillRule::NonZero, icon_color));
    }
    output.append(Geometry::stroke(path, StrokeStyle::round(2.0), icon_color));
}
