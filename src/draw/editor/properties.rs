use super::{Editor, HistoryEntry, Interaction};
use crate::config::{MIN_OPACITY, SizeRange};
use crate::draw::scene::{ElementKind, Style, tool_for};
use crate::tool::Tool;

#[derive(Default)]
pub(in crate::draw) struct Adjustment {
    pub(in crate::draw) changed: bool,
    pub(in crate::draw) feedback: Option<String>,
    pub(in crate::draw) hit_stop: bool,
}

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::draw) enum Property {
    Size,
    Opacity,
    Roundness,
}

impl Property {
    fn supported(self, tool: Tool) -> bool {
        match self {
            Self::Size => tool != Tool::Select,
            Self::Opacity => tool.supports_opacity(),
            Self::Roundness => tool.supports_roundness(),
        }
    }

    fn tool_value(self, properties: &mut ToolProperties) -> &mut f32 {
        match self {
            Self::Size => &mut properties.size,
            Self::Opacity => &mut properties.opacity,
            Self::Roundness => &mut properties.roundness,
        }
    }

    fn style_value(self, style: &mut Style) -> &mut f32 {
        match self {
            Self::Size => &mut style.size,
            Self::Opacity => &mut style.color[3],
            Self::Roundness => &mut style.roundness,
        }
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
    pub(super) fn reload(
        &mut self,
        previous: &Self,
        next: &Self,
        ranges: &std::collections::BTreeMap<Tool, SizeRange>,
    ) {
        for (index, tool) in Tool::SIZED.into_iter().enumerate() {
            let properties = &mut self.0[index];
            let previous = previous.0[index];
            let next = next.0[index];
            if previous.size != next.size {
                properties.size = next.size;
            }
            properties.size = ranges[&tool].clamp(properties.size);
            if previous.opacity != next.opacity {
                properties.opacity = next.opacity;
            }
            if previous.roundness != next.roundness {
                properties.roundness = next.roundness;
            }
            if previous.filled != next.filled {
                properties.filled = next.filled;
            }
        }
    }

    pub(super) fn new(
        stroke_size: f32,
        default_opacity: f32,
        default_fill_shapes: bool,
        defaults: &crate::config::ToolDefaults,
        size_ranges: &std::collections::BTreeMap<Tool, SizeRange>,
    ) -> Self {
        let fallback = crate::config::PropertyDefaults::default();
        let properties = |tool: Tool| {
            let configured = defaults.get(&tool).unwrap_or(&fallback);
            let size_range = size_ranges
                .get(&tool)
                .expect("adjustable tools have size ranges");
            let size = configured.size.unwrap_or_else(|| {
                tool.initial_size(stroke_size)
                    .expect("adjustable tools have sizes")
            });
            ToolProperties {
                size: size_range.clamp(size),
                opacity: configured.opacity.unwrap_or(default_opacity),
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
            return self.adjust_selected(false, |kind, style| {
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
        if !self.tool.supports_fill() && self.tool != Tool::Text {
            return Adjustment::default();
        }
        let properties = self
            .properties_mut(self.tool)
            .expect("fillable tools have adjustable properties");
        let filled = !properties.filled;
        properties.filled = filled;
        self.sync_active_style();
        Adjustment {
            changed: true,
            feedback: Some(if self.tool == Tool::Text {
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
        let step = self.property_stepper(property, steps);
        if let Some(edit) = self.text_edit_mut() {
            let value = *property.style_value(&mut edit.style);
            let (adjustment, label) = step(Tool::Text, value);
            let changed = value != adjustment.value;
            if changed {
                match property {
                    Property::Size => edit.set_size(adjustment.value),
                    _ => *property.style_value(&mut edit.style) = adjustment.value,
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
            let mut hit_stop = false;
            let mut adjustment = self.adjust_selected(true, |kind, style| {
                let tool = tool_for(kind);
                if !property.supported(tool) {
                    return None;
                }
                let value = property.style_value(style);
                let (adjustment, label) = step(tool, *value);
                *value = adjustment.value;
                hit_stop |= adjustment.hit_stop;
                Some(label)
            });
            adjustment.hit_stop = hit_stop;
            return adjustment;
        }
        let tool = self.tool;
        if !property.supported(tool) {
            return Adjustment::default();
        }
        let properties = self
            .properties_mut(tool)
            .expect("supported tools have adjustable properties");
        let value = property.tool_value(properties);
        let (adjustment, label) = step(tool, *value);
        let changed = *value != adjustment.value;
        if changed {
            *value = adjustment.value;
            self.sync_active_style();
        }
        Adjustment {
            changed,
            feedback: Some(label),
            hit_stop: adjustment.hit_stop,
        }
    }

    /// Steps a tool's property value without borrowing the editor.
    fn property_stepper(
        &self,
        property: Property,
        steps: f32,
    ) -> impl Fn(Tool, f32) -> (StepAdjustment, String) + use<> {
        let defaults = self.default_tool_properties;
        let size_ranges = self.size_ranges.clone();
        let increments = self.steps.clone();
        let stop_sets = self.stops.clone();
        move |tool, value| {
            let mut properties = *defaults
                .properties(tool)
                .expect("adjustable tools have defaults");
            let default = *property.tool_value(&mut properties);
            let (increment, stops) = (&increments[&tool], &stop_sets[&tool]);
            let (min, max, increment, stops) = match property {
                Property::Size => {
                    let range = &size_ranges[&tool];
                    (range.min(), range.max(), increment.size, &stops.size)
                }
                Property::Opacity => (MIN_OPACITY, 1.0, increment.opacity, &stops.opacity),
                Property::Roundness => (0.0, 1.0, increment.roundness, &stops.roundness),
            };
            let adjustment = stepped_with_stops(value, default, steps, increment, min, max, stops);
            let label = if property == Property::Size {
                size_label(adjustment.value, default)
            } else {
                percent_label(adjustment.value, default)
            };
            (adjustment, label)
        }
    }

    fn adjust_selected(
        &mut self,
        merge: bool,
        mut adjust: impl FnMut(&ElementKind, &mut Style) -> Option<String>,
    ) -> Adjustment {
        if let Some(Interaction::Resizing {
            properties,
            current,
            ..
        }) = &mut self.interaction
        {
            let mut style = current.style;
            let feedback = adjust(&current.kind, &mut style);
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
            if let Some(label) = adjust(&element.kind, &mut style) {
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

    pub(in crate::draw) fn apply_rgba(&mut self, rgba: [f32; 4]) -> bool {
        let mut changed = self.style.color != rgba;
        if let Some(properties) = self.properties_mut(self.color_tool()) {
            changed |= properties.opacity != rgba[3];
            properties.opacity = rgba[3];
        }
        self.style.color = rgba;
        self.update_live_stroke_style();
        if let Some(edit) = self.text_edit_mut() {
            changed |= edit.style.color != rgba;
            edit.style.color = rgba;
            return changed;
        }
        if self.selected.is_empty() {
            return changed;
        }
        changed |= self
            .adjust_selected(false, |_, style| {
                style.color = rgba;
                None
            })
            .changed;
        changed
    }

    pub(super) fn properties(&self, tool: Tool) -> Option<&ToolProperties> {
        self.tool_properties.properties(tool)
    }

    fn properties_mut(&mut self, tool: Tool) -> Option<&mut ToolProperties> {
        self.tool_properties.properties_mut(tool)
    }

    pub(super) fn style_for(&self, tool: Tool) -> Style {
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

    pub(super) fn sync_active_style(&mut self) {
        self.style = self.style_for(self.tool);
        self.update_live_stroke_style();
    }

    fn update_live_stroke_style(&mut self) {
        let style = self.style_for(Tool::Pen);
        if let Some(Interaction::Freehand(stroke)) = &mut self.interaction {
            stroke.update_style(style);
        }
    }
}

fn supports_fill(kind: &ElementKind) -> bool {
    tool_for(kind).supports_fill() || matches!(kind, ElementKind::Text { .. })
}
