use super::super::MIN_ERASER_WIDTH;
use super::super::scene::{ElementKind, Style, default_roundness};
use super::super::tool::Tool;
use super::{Damage, Editor, HistoryEntry, Interaction};

pub(super) const MIN_STROKE_WIDTH: f32 = 1.0;
pub(super) const MAX_STROKE_WIDTH: f32 = 64.0;
const MIN_OPACITY: f32 = 0.05;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 192.0;
pub(super) const DEFAULT_ERASER_WIDTH: f32 = 10.0;
pub(super) const DEFAULT_TEXT_SIZE: f32 = 20.0;

fn stroke_size_label(value: f32, default: f32) -> String {
    let suffix = if value == default { " · default" } else { "" };
    format!("{value:.1} px{suffix}")
}

fn text_size_label(value: f32, default: f32) -> String {
    let suffix = if value == default { " · default" } else { "" };
    format!("{value:.0} px{suffix}")
}

fn percent_label(value: f32, default: f32) -> String {
    let suffix = if value == default { " · default" } else { "" };
    format!("{:.0}%{suffix}", value * 100.0)
}

fn fill_label(filled: bool) -> String {
    format!("Fill · {}", if filled { "solid" } else { "outline" })
}

fn stepped_size(value: f32, default: f32, steps: f32, increment: f32, min: f32, max: f32) -> f32 {
    let offset = (value - default) / increment;
    let aligned = if steps.is_sign_positive() {
        (offset + 1e-4).floor()
    } else {
        (offset - 1e-4).ceil()
    };
    (default + (aligned + steps) * increment).clamp(min, max)
}

#[derive(Clone, Copy)]
pub(super) struct ToolProperties {
    pub size: f32,
    pub opacity: f32,
    pub roundness: f32,
    pub filled: bool,
}

#[derive(Clone)]
pub(super) struct ToolPropertySet {
    pen: ToolProperties,
    line: ToolProperties,
    arrow: ToolProperties,
    triangle: ToolProperties,
    rectangle: ToolProperties,
    ellipse: ToolProperties,
    text: ToolProperties,
    eraser: ToolProperties,
}

impl ToolPropertySet {
    pub(super) fn new(
        stroke_width: f32,
        default_fill_shapes: bool,
        defaults: &crate::config::ToolDefaults,
    ) -> Self {
        let properties = |tool: Tool, size, filled| {
            let configured = defaults.get(&tool).copied().unwrap_or_default();
            ToolProperties {
                size: configured.size.unwrap_or(size),
                opacity: configured.opacity.unwrap_or(1.0),
                roundness: configured
                    .roundness
                    .unwrap_or_else(|| tool.initial_roundness()),
                filled: configured.filled.unwrap_or(filled),
            }
        };
        Self {
            pen: properties(Tool::Pen, stroke_width, false),
            line: properties(Tool::Line, stroke_width, false),
            arrow: properties(Tool::Arrow, stroke_width, false),
            triangle: properties(Tool::Triangle, stroke_width, default_fill_shapes),
            rectangle: properties(Tool::Rectangle, stroke_width, default_fill_shapes),
            ellipse: properties(Tool::Ellipse, stroke_width, default_fill_shapes),
            text: properties(Tool::Text, DEFAULT_TEXT_SIZE, false),
            eraser: properties(Tool::Eraser, DEFAULT_ERASER_WIDTH, false),
        }
    }

    pub(super) fn properties(&self, tool: Tool) -> Option<&ToolProperties> {
        Some(match tool {
            Tool::Pen => &self.pen,
            Tool::Line => &self.line,
            Tool::Arrow => &self.arrow,
            Tool::Triangle => &self.triangle,
            Tool::Rectangle => &self.rectangle,
            Tool::Ellipse => &self.ellipse,
            Tool::Text => &self.text,
            Tool::Eraser => &self.eraser,
            Tool::Select => return None,
        })
    }

    fn properties_mut(&mut self, tool: Tool) -> Option<&mut ToolProperties> {
        Some(match tool {
            Tool::Pen => &mut self.pen,
            Tool::Line => &mut self.line,
            Tool::Arrow => &mut self.arrow,
            Tool::Triangle => &mut self.triangle,
            Tool::Rectangle => &mut self.rectangle,
            Tool::Ellipse => &mut self.ellipse,
            Tool::Text => &mut self.text,
            Tool::Eraser => &mut self.eraser,
            Tool::Select => return None,
        })
    }
}

impl Editor {
    pub(super) fn toggle_fill(&mut self) -> (Damage, String) {
        if !self.selected.is_empty() {
            let fill = self
                .selected
                .iter()
                .filter_map(|id| self.element(*id))
                .filter(|element| fillable(&element.kind))
                .any(|element| !element.style.filled);
            return self.adjust_selected(|kind, style| {
                fillable(kind).then(|| {
                    style.filled = fill;
                    fill_label(fill)
                })
            });
        }
        if !self.tool.supports_fill() {
            return (Damage::None, String::new());
        }
        let properties = self
            .properties_mut(self.tool)
            .expect("fillable tools have adjustable properties");
        let filled = !properties.filled;
        properties.filled = filled;
        self.sync_active_style();
        (Damage::Preview, fill_label(filled))
    }

    pub(super) fn tool_fill(&self, tool: Tool) -> bool {
        self.properties(tool)
            .is_some_and(|properties| properties.filled)
    }

    pub(in crate::state::draw) fn adjust_size(&mut self, steps: f32) -> (Damage, String) {
        if steps == 0.0 {
            return (Damage::None, String::new());
        }
        let default_text_size = self.default_text_size;
        if let Some(edit) = self.text_edit_mut() {
            let font_size = stepped_size(
                edit.font_size,
                default_text_size,
                steps,
                1.0,
                MIN_FONT_SIZE,
                MAX_FONT_SIZE,
            );
            let label = text_size_label(font_size, default_text_size);
            if font_size == edit.font_size {
                return (Damage::Preview, label);
            }
            edit.font_size = font_size;
            return (Damage::Preview, label);
        }
        if !self.selected.is_empty() {
            let default_width = self.default_width;
            return self.adjust_selected(|kind, style| {
                Some(match kind {
                    ElementKind::Text { font_size, .. } => {
                        *font_size = stepped_size(
                            *font_size,
                            default_text_size,
                            steps,
                            1.0,
                            MIN_FONT_SIZE,
                            MAX_FONT_SIZE,
                        );
                        text_size_label(*font_size, default_text_size)
                    }
                    _ => {
                        style.width = stepped_size(
                            style.width,
                            default_width,
                            steps,
                            1.0,
                            MIN_STROKE_WIDTH,
                            MAX_STROKE_WIDTH,
                        );
                        stroke_size_label(style.width, default_width)
                    }
                })
            });
        }
        if self.tool == Tool::Text {
            let properties = self
                .properties_mut(Tool::Text)
                .expect("text must have adjustable properties");
            let size = stepped_size(
                properties.size,
                default_text_size,
                steps,
                1.0,
                MIN_FONT_SIZE,
                MAX_FONT_SIZE,
            );
            let label = text_size_label(size, default_text_size);
            if size == properties.size {
                return (Damage::Preview, label);
            }
            properties.size = size;
            (Damage::Preview, label)
        } else {
            let tool = self.tool;
            let Some(default) = self.default_size(tool) else {
                return (Damage::None, String::new());
            };
            let minimum = match tool {
                Tool::Eraser => MIN_ERASER_WIDTH,
                _ => MIN_STROKE_WIDTH,
            };
            let properties = self
                .properties_mut(tool)
                .expect("tools with a default size have adjustable properties");
            let width = stepped_size(
                properties.size,
                default,
                steps,
                1.0,
                minimum,
                MAX_STROKE_WIDTH,
            );
            let label = stroke_size_label(width, default);
            if width == properties.size {
                return (Damage::Preview, label);
            }
            properties.size = width;
            self.sync_active_style();
            let damage = self.update_live_stroke_style();
            (damage.max(Damage::Preview), label)
        }
    }

    pub(in crate::state::draw) fn adjust_opacity(&mut self, steps: f32) -> (Damage, String) {
        if steps == 0.0 {
            return (Damage::None, String::new());
        }
        let default_text_opacity = self.default_properties(Tool::Text).opacity;
        if let Some(edit) = self.text_edit_mut() {
            let opacity = stepped_size(
                edit.style.color[3],
                default_text_opacity,
                steps,
                0.01,
                MIN_OPACITY,
                1.0,
            );
            let label = percent_label(opacity, default_text_opacity);
            if opacity == edit.style.color[3] {
                return (Damage::Preview, label);
            }
            edit.style.color[3] = opacity;
            return (Damage::Preview, label);
        }
        if self.selected.is_empty() {
            if self.tool == Tool::Eraser {
                return (Damage::None, String::new());
            }
            let tool = self.tool;
            let default = self.default_properties(tool).opacity;
            let Some(properties) = self.properties_mut(tool) else {
                return (Damage::None, String::new());
            };
            let opacity = stepped_size(properties.opacity, default, steps, 0.01, MIN_OPACITY, 1.0);
            let label = percent_label(opacity, default);
            if opacity == properties.opacity {
                return (Damage::Preview, label);
            }
            properties.opacity = opacity;
            self.sync_active_style();
            let damage = self.update_live_stroke_style();
            return (damage.max(Damage::Preview), label);
        }
        self.adjust_selected(|_, style| {
            style.color[3] = stepped_size(style.color[3], 1.0, steps, 0.01, MIN_OPACITY, 1.0);
            Some(percent_label(style.color[3], 1.0))
        })
    }

    pub(in crate::state::draw) fn adjust_roundness(&mut self, steps: f32) -> (Damage, String) {
        if steps == 0.0 {
            return (Damage::None, String::new());
        }
        if self.selected.is_empty() {
            let tool = self.tool;
            if tool.default_roundness().is_none() {
                return (Damage::None, String::new());
            }
            let default = self.default_properties(tool).roundness;
            let properties = self
                .properties_mut(tool)
                .expect("tools with roundness have adjustable properties");
            let roundness = stepped_size(properties.roundness, default, steps, 0.01, 0.0, 1.0);
            let label = percent_label(roundness, default);
            if roundness == properties.roundness {
                return (Damage::Preview, label);
            }
            properties.roundness = roundness;
            self.sync_active_style();
            let damage = self.update_live_stroke_style();
            return (damage.max(Damage::Preview), label);
        }
        self.adjust_selected(|kind, style| {
            let default = default_roundness(kind)?;
            style.roundness = stepped_size(style.roundness, default, steps, 0.01, 0.0, 1.0);
            Some(percent_label(style.roundness, default))
        })
    }

    fn adjust_selected(
        &mut self,
        mut adjust: impl FnMut(&mut ElementKind, &mut Style) -> Option<String>,
    ) -> (Damage, String) {
        let ids = self.selected.clone();
        let mut updates = Vec::with_capacity(ids.len());
        let mut feedback = String::new();
        for id in ids {
            let Some(element) = self.element_mut(id) else {
                continue;
            };
            let mut kind = element.kind.clone();
            let mut style = element.style;
            let Some(label) = adjust(&mut kind, &mut style) else {
                continue;
            };
            feedback = label;
            if kind != element.kind || style != element.style {
                let (kind, style) = element.replace(kind, style);
                updates.push((id, kind, style));
            }
        }
        if updates.is_empty() {
            return if feedback.is_empty() {
                (Damage::None, feedback)
            } else {
                (Damage::Preview, feedback)
            };
        }
        self.history.record(HistoryEntry::Update(updates));
        (Damage::Scene, feedback)
    }

    fn update_live_stroke_style(&mut self) -> Damage {
        match &mut self.interaction {
            Some(Interaction::Freehand(stroke)) => {
                stroke.update_style(self.style);
                Damage::Preview
            }
            _ => Damage::None,
        }
    }

    pub(super) fn apply_rgb(&mut self, rgb: [f32; 3]) -> Damage {
        self.style.color[..3].copy_from_slice(&rgb);
        if let Some(edit) = self.text_edit_mut() {
            edit.style.color[..3].copy_from_slice(&rgb);
            return Damage::Preview;
        }
        if self.selected.is_empty() {
            return Damage::Preview;
        }
        let ids = self.selected.clone();
        let mut elements = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(element) = self.element_mut(id) else {
                continue;
            };
            let mut style = element.style;
            if style.color[..3] == rgb {
                continue;
            }
            style.color[..3].copy_from_slice(&rgb);
            let kind = element.kind.clone();
            let (kind, style) = element.replace(kind, style);
            elements.push((id, kind, style));
        }
        if elements.is_empty() {
            return Damage::Preview;
        }
        self.history.record(HistoryEntry::Update(elements));
        Damage::Scene
    }

    pub(super) fn properties(&self, tool: Tool) -> Option<&ToolProperties> {
        self.tool_properties.properties(tool)
    }

    fn properties_mut(&mut self, tool: Tool) -> Option<&mut ToolProperties> {
        self.tool_properties.properties_mut(tool)
    }

    fn default_size(&self, tool: Tool) -> Option<f32> {
        self.default_tool_properties
            .properties(tool)
            .map(|properties| properties.size)
    }

    fn default_properties(&self, tool: Tool) -> &ToolProperties {
        self.default_tool_properties
            .properties(tool)
            .expect("tools with adjustable properties have defaults")
    }

    fn style_for(&self, tool: Tool) -> Style {
        let Some(properties) = self.properties(tool) else {
            return self.style;
        };
        let mut style = self.style;
        if tool != Tool::Text {
            style.width = properties.size;
        }
        style.color[3] = properties.opacity;
        style.roundness = properties.roundness;
        style.filled = properties.filled;
        style
    }

    pub(super) fn width_for(&self, tool: Tool) -> f32 {
        self.properties(tool)
            .map_or(self.style.width, |properties| properties.size)
    }

    pub(super) fn sync_active_style(&mut self) {
        self.style = self.style_for(self.tool);
    }
}

fn fillable(kind: &ElementKind) -> bool {
    matches!(
        kind,
        ElementKind::Triangle { .. } | ElementKind::Rectangle { .. } | ElementKind::Ellipse { .. }
    )
}
