use super::{Editor, HistoryEntry, Interaction};
use crate::Rgba;
use crate::cli::ToolSetting;
use crate::config::{Property, ToolDefinition, ToolProperties};
use crate::draw::scene::{ElementKind, Style, tool_for};
use crate::tool::{Primitive, ToolId};

#[derive(Default)]
pub(in crate::draw) struct Adjustment {
    pub(in crate::draw) changed: bool,
    pub(in crate::draw) feedback: Option<String>,
    pub(in crate::draw) hit_stop: bool,
}

fn size_label(value: f32, default: f32) -> String {
    let suffix = if value == default { " (default)" } else { "" };
    format!("{value} px{suffix}")
}

fn percent_label(value: f32, default: f32) -> String {
    let suffix = if value == default { " (default)" } else { "" };
    format!("{:.0}%{suffix}", value * 100.0)
}

fn fill_label(filled: bool) -> String {
    format!("Fill {}", if filled { "solid" } else { "outline" })
}

fn background_label(background: bool) -> String {
    format!("Background {}", if background { "on" } else { "off" })
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

struct StepAdjustment {
    value: f32,
    hit_stop: bool,
}

fn stepped_with_stops(
    value: f32,
    default: f32,
    steps: f32,
    increment: f32,
    min: f32,
    max: f32,
    stops: &[f32],
) -> StepAdjustment {
    let at_stop = value == default || stops.contains(&value);
    let target = if at_stop {
        (value + steps * increment).clamp(min, max)
    } else {
        stepped_value(value, default, steps, increment, min, max)
    };
    let tolerance = increment * 1e-4;
    let stops = std::iter::once(default).chain(stops.iter().copied());
    let stop = if target > value {
        stops
            .filter(|stop| *stop > value && *stop <= target + tolerance)
            .min_by(f32::total_cmp)
    } else if target < value {
        stops
            .filter(|stop| *stop < value && *stop >= target - tolerance)
            .max_by(f32::total_cmp)
    } else {
        None
    };
    StepAdjustment {
        value: stop.unwrap_or(target),
        hit_stop: stop.is_some(),
    }
}

impl Property {
    fn style_value(self, style: &mut Style) -> &mut f32 {
        match self {
            Self::Size => &mut style.size,
            Self::Opacity => &mut style.color[3],
            Self::Roundness => &mut style.roundness,
        }
    }

    fn tool_value(self, properties: &mut ToolProperties) -> &mut f32 {
        match self {
            Self::Size => &mut properties.size,
            Self::Opacity => &mut properties.opacity,
            Self::Roundness => &mut properties.roundness,
        }
    }

    fn adjust(self, tool: &ToolDefinition, value: f32, steps: f32) -> (StepAdjustment, String) {
        let mut properties = tool.properties;
        let default = *self.tool_value(&mut properties);
        let (min, max) = self.bounds(tool.size_range);
        let (increment, stops) = (*tool.steps.get(self), tool.stops.get(self));
        let adjustment = stepped_with_stops(value, default, steps, increment, min, max, stops);
        let label = match self {
            Self::Size => size_label(adjustment.value, default),
            Self::Opacity | Self::Roundness => percent_label(adjustment.value, default),
        };
        (adjustment, label)
    }
}

fn recolor(color: &mut Rgba, [r, g, b, alpha]: Rgba) {
    color[..3].copy_from_slice(&[r, g, b]);
    if alpha < 1.0 {
        color[3] = alpha;
    }
}

impl Editor {
    pub(in crate::draw) fn set_tool_setting(
        &mut self,
        target: Option<&str>,
        setting: ToolSetting,
    ) -> Result<bool, String> {
        let id = target
            .map(|name| self.resolve_tool(name))
            .transpose()?
            .unwrap_or_else(|| {
                if matches!(setting, ToolSetting::Color { .. }) {
                    self.color_tool()
                } else {
                    self.tool
                }
            });
        let definition = &self.tools[id.0];
        let primitive = definition.primitive;
        let supported = match setting {
            ToolSetting::Size { .. } => Property::Size.supported(primitive),
            ToolSetting::Opacity { .. } => Property::Opacity.supported(primitive),
            ToolSetting::Color { .. } => true,
            ToolSetting::Roundness { .. } => Property::Roundness.supported(primitive),
            ToolSetting::Filled { .. } => primitive.supports_fill() || primitive == Primitive::Text,
        };
        if !supported {
            return Err(format!("property is not applicable to {}", definition.id));
        }
        let properties = &mut self.tool_properties[id.0];
        let before = *properties;
        let before_color = self.color;
        match setting {
            ToolSetting::Size { value } => {
                if !definition.size_range.contains(value) {
                    return Err(format!(
                        "{} size must be between {} and {}",
                        definition.id, definition.size_range.min, definition.size_range.max
                    ));
                }
                properties.size = value;
            }
            ToolSetting::Opacity { value } => properties.opacity = value,
            ToolSetting::Roundness { value } => properties.roundness = value,
            ToolSetting::Filled { value } => properties.filled = value,
            ToolSetting::Color { value } => {
                let [r, g, b, alpha] = crate::color_to_srgb(value);
                self.color = [r, g, b];
                if alpha < 1.0 && primitive.supports_opacity() {
                    properties.opacity = alpha;
                }
            }
        }
        Ok(before != *properties || before_color != self.color)
    }

    pub(super) fn toggle_fill(&mut self) -> Adjustment {
        if let Some(edit) = self.text_edit_mut() {
            edit.style.filled = !edit.style.filled;
            return Adjustment {
                changed: true,
                feedback: Some(background_label(edit.style.filled)),
                ..Default::default()
            };
        }
        if !self.selected.is_empty() {
            let enable = if let Some(Interaction::Resizing { current, .. }) = &self.interaction {
                !current.style.filled
            } else {
                self.selected
                    .iter()
                    .filter_map(|id| self.element(*id))
                    .filter(|element| supports_fill(&element.kind))
                    .any(|element| !element.style.filled)
            };
            return self.adjust_selected(false, |kind, _, style| {
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
        if !self.primitive().supports_fill() && self.primitive() != Primitive::Text {
            return Adjustment::default();
        }
        let properties = &mut self.tool_properties[self.tool.0];
        let filled = !properties.filled;
        properties.filled = filled;
        self.update_live_style(self.tool, |style| style.filled = filled);
        Adjustment {
            changed: true,
            feedback: Some(if self.primitive() == Primitive::Text {
                background_label(filled)
            } else {
                fill_label(filled)
            }),
            ..Default::default()
        }
    }

    pub(in crate::draw) fn adjust(&mut self, property: Property, steps: f32) -> Adjustment {
        if steps == 0.0 {
            return Adjustment::default();
        }
        if let Some(Interaction::EditingText(preset, edit)) = &mut self.interaction {
            let id = ToolDefinition::preset(&self.tools, preset, Primitive::Text);
            let value = property.style_value(&mut edit.style);
            let (adjustment, label) = property.adjust(&self.tools[id.0], *value, steps);
            let changed = *value != adjustment.value;
            if changed {
                match property {
                    Property::Size => edit.set_size(adjustment.value),
                    _ => *value = adjustment.value,
                }
            }
            return Adjustment {
                changed,
                feedback: Some(label),
                hit_stop: adjustment.hit_stop,
            };
        }
        if !self.selected.is_empty() {
            if self.adjusted.replace(property) != Some(property) {
                self.document.history.seal();
            }
            let tools = self.tools.clone();
            let mut hit_stop = false;
            let mut adjustment = self.adjust_selected(true, |kind, preset, style| {
                let primitive = tool_for(kind);
                if !property.supported(primitive) {
                    return None;
                }
                let id = ToolDefinition::preset(&tools, preset, primitive);
                let value = property.style_value(style);
                let (adjustment, label) = property.adjust(&tools[id.0], *value, steps);
                *value = adjustment.value;
                hit_stop |= adjustment.hit_stop;
                Some(label)
            });
            adjustment.hit_stop = hit_stop;
            return adjustment;
        }
        let tool = self.primitive();
        if !property.supported(tool) {
            return Adjustment::default();
        }
        let value = property.tool_value(&mut self.tool_properties[self.tool.0]);
        let (adjustment, label) = property.adjust(&self.tools[self.tool.0], *value, steps);
        let changed = *value != adjustment.value;
        if changed {
            *value = adjustment.value;
            self.update_live_style(self.tool, |style| {
                *property.style_value(style) = adjustment.value;
            });
        }
        Adjustment {
            changed,
            feedback: Some(label),
            hit_stop: adjustment.hit_stop,
        }
    }

    fn adjust_selected(
        &mut self,
        merge: bool,
        mut adjust: impl FnMut(&ElementKind, &str, &mut Style) -> Option<String>,
    ) -> Adjustment {
        if let Some(Interaction::Resizing {
            preset,
            properties,
            current,
            ..
        }) = &mut self.interaction
        {
            let mut style = current.style;
            let feedback = adjust(&current.kind, preset, &mut style);
            let changed = style != current.style;
            if changed {
                // Text resizing also changes font size; retain only the user's adjustment.
                *properties = Style {
                    size: properties.size + (style.size - current.style.size),
                    ..style
                };
                current.set_style(style);
            }
            return Adjustment {
                changed,
                feedback,
                ..Default::default()
            };
        }
        let ids = self.selected.clone();
        let mut updates = Vec::with_capacity(ids.len());
        let mut feedback = None;
        for id in ids {
            let Some(element) = self.element_mut(id) else {
                continue;
            };
            let mut style = element.style;
            if let Some(label) = adjust(&element.kind, &element.preset, &mut style) {
                feedback = Some(label);
            }
            if style != element.style {
                let (kind, style) = element.replace(element.kind.clone(), style);
                updates.push((id, kind, style));
            }
        }
        let changed = !updates.is_empty();
        if changed {
            let entry = HistoryEntry::Update(updates);
            if merge {
                self.document.history.merge(entry);
            } else {
                self.document.history.record(entry);
            }
        }
        Adjustment {
            changed,
            feedback,
            ..Default::default()
        }
    }

    pub(super) fn apply_color(&mut self, color: Rgba) {
        let [r, g, b, alpha] = color;
        self.color = [r, g, b];
        let id = self.color_tool();
        if alpha < 1.0 && self.tools[id.0].primitive.supports_opacity() {
            self.tool_properties[id.0].opacity = alpha;
        }
        if let Some(edit) = self.text_edit_mut() {
            recolor(&mut edit.style.color, color);
        } else if !self.selected.is_empty() {
            self.adjust_selected(false, |_, _, style| {
                recolor(&mut style.color, color);
                None
            });
        }
    }

    pub(in crate::draw) fn named_tool_style(
        &self,
        id: Option<&str>,
    ) -> Result<(Primitive, Style), String> {
        let id = id
            .map(|name| self.resolve_tool(name))
            .transpose()?
            .unwrap_or(self.tool);
        Ok((self.tools[id.0].primitive, self.tool_style(id)))
    }

    fn update_live_style(&mut self, tool: ToolId, update: impl FnOnce(&mut Style)) {
        let mut eraser = self.tool_style(tool);
        match &mut self.interaction {
            Some(Interaction::Freehand(current, stroke)) if *current == tool => {
                stroke.update_style(update);
            }
            Some(Interaction::Drawing {
                preset: current,
                style,
                ..
            }) if *current == tool => update(style),
            Some(Interaction::Erasing {
                tool: current,
                size,
                ..
            }) if *current == tool => {
                eraser.size = *size;
                update(&mut eraser);
                *size = eraser.size;
            }
            _ => {}
        }
    }
}

fn supports_fill(kind: &ElementKind) -> bool {
    tool_for(kind).supports_fill() || matches!(kind, ElementKind::Text { .. })
}
