use super::super::scene::{ElementKind, Style, default_roundness, tool_for};
use super::super::tool::Tool;
use super::{Damage, Editor, HistoryEntry, Interaction};
use crate::config::SizeRange;

const MIN_OPACITY: f32 = 0.05;

fn size_label(value: f32, default: f32) -> String {
    let suffix = if value == default { " · default" } else { "" };
    format!("{value} px{suffix}")
}

fn percent_label(value: f32, default: f32) -> String {
    let suffix = if value == default { " · default" } else { "" };
    format!("{:.0}%{suffix}", value * 100.0)
}

fn fill_label(filled: bool) -> String {
    format!("Fill · {}", if filled { "solid" } else { "outline" })
}

fn background_label(background: bool) -> String {
    format!("Background · {}", if background { "on" } else { "off" })
}

fn adjust_percent(value: &mut f32, default: f32, steps: f32, min: f32) -> (Damage, String) {
    *value = stepped_value(*value, default, steps, 0.01, min, 1.0);
    (Damage::Preview, percent_label(*value, default))
}

fn stepped_value(value: f32, default: f32, steps: f32, increment: f32, min: f32, max: f32) -> f32 {
    let offset = (value - default) / increment;
    let aligned = if steps.is_sign_positive() {
        (offset + 1e-4).floor()
    } else {
        (offset - 1e-4).ceil()
    };
    (default + (aligned + steps) * increment).clamp(min, max)
}

struct SizeAdjustment {
    value: f32,
    hit_stop: bool,
}

fn stepped_size(value: f32, default: f32, steps: f32, range: &SizeRange) -> SizeAdjustment {
    let at_stop = value == default || range.stops().contains(&value);
    let target = if at_stop {
        range.clamp(value + steps * range.step())
    } else {
        stepped_value(
            value,
            default,
            steps,
            range.step(),
            range.min(),
            range.max(),
        )
    };
    let stops = std::iter::once(default).chain(range.stops().iter().copied());
    let stop = if target > value {
        stops
            .filter(|stop| *stop > value && *stop <= target)
            .min_by(f32::total_cmp)
    } else {
        stops
            .filter(|stop| *stop < value && *stop >= target)
            .max_by(f32::total_cmp)
    };
    SizeAdjustment {
        value: stop.unwrap_or(target),
        hit_stop: stop.is_some(),
    }
}

#[derive(Clone, Copy)]
pub(super) struct ToolProperties {
    pub size: f32,
    pub opacity: f32,
    pub roundness: f32,
    pub filled: bool,
}

#[derive(Clone, Copy)]
pub(super) struct ToolPropertySet([ToolProperties; Tool::SIZED.len()]);

impl ToolPropertySet {
    pub(super) fn new(
        stroke_size: f32,
        default_fill_shapes: bool,
        defaults: &crate::config::ToolDefaults,
        size_ranges: &std::collections::BTreeMap<Tool, SizeRange>,
    ) -> Self {
        let properties = |tool: Tool| {
            let configured = defaults.get(&tool).cloned().unwrap_or_default();
            let size_range = size_ranges
                .get(&tool)
                .expect("adjustable tools have size ranges");
            let size = configured.size.unwrap_or_else(|| {
                tool.initial_size(stroke_size)
                    .expect("adjustable tools have sizes")
            });
            ToolProperties {
                size: size_range.clamp(size),
                opacity: configured.opacity.unwrap_or(1.0),
                roundness: configured
                    .roundness
                    .unwrap_or_else(|| tool.initial_roundness()),
                filled: if tool == Tool::Text {
                    configured.background.unwrap_or(false)
                } else {
                    configured
                        .filled
                        .unwrap_or(default_fill_shapes && tool.supports_fill())
                },
            }
        };
        Self(Tool::SIZED.map(properties))
    }

    pub(super) fn properties(&self, tool: Tool) -> Option<&ToolProperties> {
        self.0
            .get(Tool::SIZED.iter().position(|item| *item == tool)?)
    }

    fn properties_mut(&mut self, tool: Tool) -> Option<&mut ToolProperties> {
        self.0
            .get_mut(Tool::SIZED.iter().position(|item| *item == tool)?)
    }
}

impl Editor {
    pub(super) fn toggle_fill(&mut self) -> (Damage, String) {
        if let Some(edit) = self.text_edit_mut() {
            edit.style.filled = !edit.style.filled;
            return (Damage::Preview, background_label(edit.style.filled));
        }
        if !self.selected.is_empty() {
            let enable = self
                .selected
                .iter()
                .filter_map(|id| self.element(*id))
                .filter(|element| supports_fill(&element.kind))
                .any(|element| !element.style.filled);
            return self.adjust_selected(|kind, style| {
                supports_fill(kind).then(|| {
                    style.filled = enable;
                    if matches!(kind, ElementKind::Text { .. }) {
                        background_label(enable)
                    } else {
                        fill_label(enable)
                    }
                })
            });
        }
        if self.tool == Tool::Text {
            let properties = self
                .properties_mut(Tool::Text)
                .expect("text has adjustable properties");
            properties.filled = !properties.filled;
            let background = properties.filled;
            self.sync_active_style();
            return (Damage::Preview, background_label(background));
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

    pub(in crate::state::draw) fn adjust_size(&mut self, steps: f32) -> (Damage, String, bool) {
        if steps == 0.0 {
            return (Damage::None, String::new(), false);
        }
        let default_text_size = self
            .default_size(Tool::Text)
            .expect("text must have an adjustable size");
        let text_size_range = self
            .size_ranges
            .get(&Tool::Text)
            .expect("text must have a size range")
            .clone();
        if let Some(edit) = self.text_edit_mut() {
            let adjustment =
                stepped_size(edit.style.size, default_text_size, steps, &text_size_range);
            let label = size_label(adjustment.value, default_text_size);
            edit.style.size = adjustment.value;
            return (Damage::Preview, label, adjustment.hit_stop);
        }
        if !self.selected.is_empty() {
            let defaults = self.default_tool_properties;
            let size_ranges = self.size_ranges.clone();
            let mut hit_stop = false;
            let (damage, feedback) = self.adjust_selected(|kind, style| {
                let tool = tool_for(kind);
                let default = defaults
                    .properties(tool)
                    .expect("element tools have adjustable properties")
                    .size;
                let size_range = size_ranges
                    .get(&tool)
                    .expect("element tools have size ranges");
                let adjustment = stepped_size(style.size, default, steps, size_range);
                style.size = adjustment.value;
                hit_stop |= adjustment.hit_stop;
                Some(size_label(style.size, default))
            });
            return (damage, feedback, hit_stop);
        }
        let tool = self.tool;
        let Some(default) = self.default_size(tool) else {
            return (Damage::None, String::new(), false);
        };
        let size_range = self
            .size_ranges
            .get(&tool)
            .expect("adjustable tools have size ranges")
            .clone();
        let properties = self
            .properties_mut(tool)
            .expect("tools with a default size have adjustable properties");
        let adjustment = stepped_size(properties.size, default, steps, &size_range);
        let label = size_label(adjustment.value, default);
        if adjustment.value == properties.size {
            return (Damage::Preview, label, adjustment.hit_stop);
        }
        properties.size = adjustment.value;
        self.sync_active_style();
        let damage = self.update_live_stroke_style();
        (damage.max(Damage::Preview), label, adjustment.hit_stop)
    }

    pub(in crate::state::draw) fn adjust_opacity(&mut self, steps: f32) -> (Damage, String) {
        if steps == 0.0 {
            return (Damage::None, String::new());
        }
        let default_text_opacity = self.default_properties(Tool::Text).opacity;
        if let Some(edit) = self.text_edit_mut() {
            return adjust_percent(
                &mut edit.style.color[3],
                default_text_opacity,
                steps,
                MIN_OPACITY,
            );
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
            let opacity = stepped_value(properties.opacity, default, steps, 0.01, MIN_OPACITY, 1.0);
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
            style.color[3] = stepped_value(style.color[3], 1.0, steps, 0.01, MIN_OPACITY, 1.0);
            Some(percent_label(style.color[3], 1.0))
        })
    }

    pub(in crate::state::draw) fn adjust_roundness(&mut self, steps: f32) -> (Damage, String) {
        if steps == 0.0 {
            return (Damage::None, String::new());
        }
        let default_text_roundness = self.default_properties(Tool::Text).roundness;
        if let Some(edit) = self.text_edit_mut() {
            return adjust_percent(
                &mut edit.style.roundness,
                default_text_roundness,
                steps,
                0.0,
            );
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
            let roundness = stepped_value(properties.roundness, default, steps, 0.01, 0.0, 1.0);
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
            style.roundness = stepped_value(style.roundness, default, steps, 0.01, 0.0, 1.0);
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
        self.apply_color(move |color| color[..3].copy_from_slice(&rgb))
    }

    pub(in crate::state::draw) fn apply_rgba(&mut self, rgba: [f32; 4]) -> Damage {
        if let Some(properties) = self.properties_mut(self.tool) {
            properties.opacity = rgba[3];
        }
        self.apply_color(move |color| *color = rgba)
    }

    fn apply_color(&mut self, apply: impl Fn(&mut [f32; 4])) -> Damage {
        apply(&mut self.style.color);
        if let Some(edit) = self.text_edit_mut() {
            apply(&mut edit.style.color);
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
            let previous = style.color;
            apply(&mut style.color);
            if style.color == previous {
                continue;
            }
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
        style.size = properties.size;
        style.color[3] = properties.opacity;
        style.roundness = properties.roundness;
        style.filled = properties.filled;
        style
    }

    pub(super) fn size_for(&self, tool: Tool) -> f32 {
        self.properties(tool)
            .map_or(self.style.size, |properties| properties.size)
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

fn supports_fill(kind: &ElementKind) -> bool {
    fillable(kind) || matches!(kind, ElementKind::Text { .. })
}
