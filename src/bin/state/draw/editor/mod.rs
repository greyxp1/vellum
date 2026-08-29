mod elements;
mod interaction;
mod properties;

use self::interaction::Interaction;
use self::properties::ToolPropertySet;
use super::history::{Entry as HistoryEntry, History};
use super::picker::{Choice, Picker, ShapeFills, choice, picker_geometry};
use super::scene::{Element, geometry};
use super::scene::{ElementId, ElementKind, EndMarker, Point, Style};
use super::selection;
pub(crate) use super::text_edit::CursorMove;
use super::text_edit::TextEdit;
use super::tool::Tool;
use super::{MAX_STROKE_WIDTH, MIN_TOOL_SIZE, Modifiers};
use crate::render::Geometry;

pub(crate) enum Action {
    Undo,
    Redo,
    SelectAll,
    ToggleEraser,
    ToggleFill,
    Delete,
    Clear,
    Cancel,
    CommitText,
    Backspace,
    BackspaceWord,
    MoveCursor(CursorMove),
    InsertText(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Damage {
    #[default]
    None,
    Preview,
    Scene,
}

impl Damage {
    pub fn merge(&mut self, other: Self) {
        *self = (*self).max(other);
    }

    pub fn changed(self) -> bool {
        self != Self::None
    }

    fn from_preview(changed: bool) -> Self {
        if changed { Self::Preview } else { Self::None }
    }

    fn from_scene(changed: bool) -> Self {
        if changed { Self::Scene } else { Self::None }
    }
}

#[derive(Default)]
pub struct EditorEffect {
    pub damage: Damage,
    pub deactivate: bool,
    pub feedback: Option<String>,
}

pub struct Editor {
    tool: Tool,
    style: Style,
    elements: Vec<Element>,
    selected: Vec<ElementId>,
    interaction: Option<Interaction>,
    history: History,
    next_id: ElementId,
    picker: Option<Picker>,
    default_width: f32,
    default_text_size: f32,
    default_tool: Tool,
    last_non_eraser_tool: Tool,
    tool_properties: ToolPropertySet,
    default_tool_properties: ToolPropertySet,
    remember_last_tool: bool,
    palette: Vec<[f32; 3]>,
}

impl Editor {
    pub fn new(
        width: f32,
        rgb: crate::Rgb,
        default_tool: Tool,
        remember_last_tool: bool,
        default_fill_shapes: bool,
        tool_defaults: &crate::config::ToolDefaults,
        palette: Vec<crate::Rgb>,
    ) -> Self {
        let width = width.clamp(MIN_TOOL_SIZE, MAX_STROKE_WIDTH);
        let default_tool_properties =
            ToolPropertySet::new(width, default_fill_shapes, tool_defaults);
        let tool_properties = default_tool_properties.clone();
        let active = tool_properties.properties(default_tool).copied();
        Self {
            tool: default_tool,
            style: Style {
                width: active.map_or(width, |properties| properties.size),
                color: [
                    rgb[0],
                    rgb[1],
                    rgb[2],
                    active.map_or(1.0, |properties| properties.opacity),
                ],
                roundness: active.map_or(0.5, |properties| properties.roundness),
                filled: active.is_some_and(|properties| properties.filled),
            },
            elements: Vec::new(),
            selected: Vec::new(),
            interaction: None,
            history: History::default(),
            next_id: 1,
            picker: None,
            default_width: width,
            default_text_size: default_tool_properties
                .properties(Tool::Text)
                .expect("text must have adjustable properties")
                .size,
            default_tool,
            last_non_eraser_tool: if default_tool == Tool::Eraser {
                Tool::Pen
            } else {
                default_tool
            },
            tool_properties,
            default_tool_properties,
            remember_last_tool,
            palette,
        }
    }

    pub fn activate(&mut self) -> Damage {
        if self.remember_last_tool || self.tool == self.default_tool {
            return Damage::None;
        }
        self.switch_tool(self.default_tool)
    }

    pub fn deactivate(&mut self) -> Damage {
        let damage = if self.is_editing_text() {
            self.commit_text()
        } else {
            let restore_scene = matches!(
                self.interaction,
                Some(Interaction::Moving { .. } | Interaction::Resizing { .. })
            );
            let changed = self.interaction.take().is_some();
            if restore_scene {
                Damage::Scene
            } else {
                Damage::from_preview(changed)
            }
        };
        let clear_preview =
            !std::mem::take(&mut self.selected).is_empty() | self.picker.take().is_some();
        damage.max(Damage::from_preview(clear_preview))
    }

    pub fn is_editing_text(&self) -> bool {
        matches!(self.interaction, Some(Interaction::EditingText(_)))
    }

    pub fn is_drawing_pen(&self) -> bool {
        matches!(self.interaction, Some(Interaction::Freehand(_)))
    }

    fn text_edit(&self) -> Option<&TextEdit> {
        match &self.interaction {
            Some(Interaction::EditingText(edit)) => Some(edit),
            _ => None,
        }
    }

    fn text_edit_mut(&mut self) -> Option<&mut TextEdit> {
        match &mut self.interaction {
            Some(Interaction::EditingText(edit)) => Some(edit),
            _ => None,
        }
    }

    pub fn current_color(&self) -> [f32; 4] {
        if let Some(edit) = self.text_edit() {
            edit.style.color
        } else if let Some(element) = self.selected.last().and_then(|id| self.element(*id)) {
            element.style.color
        } else {
            self.style.color
        }
    }

    pub fn picker_active(&self) -> bool {
        self.picker.is_some()
    }

    pub fn handle_action(&mut self, action: Action) -> EditorEffect {
        let mut effect = EditorEffect::default();
        let closed_picker = self.picker.take().is_some();
        if closed_picker && matches!(action, Action::Cancel) {
            effect.damage = Damage::Preview;
            return effect;
        }
        match action {
            Action::Undo if !self.is_editing_text() => effect.damage = self.undo(),
            Action::Redo if !self.is_editing_text() => effect.damage = self.redo(),
            Action::SelectAll => effect.damage = self.select_all(),
            Action::ToggleEraser => effect.damage = self.toggle_eraser(),
            Action::ToggleFill => {
                let (damage, feedback) = self.toggle_fill();
                effect.damage = damage;
                effect.feedback = (!feedback.is_empty()).then_some(feedback);
            }
            Action::Delete => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.damage = Damage::from_preview(edit.delete());
                } else {
                    effect.damage = self.delete_selection();
                }
            }
            Action::Clear => effect.damage = self.clear(),
            Action::Cancel => {
                let cancelled = self.cancel_interaction();
                if cancelled.changed() || !std::mem::take(&mut self.selected).is_empty() {
                    effect.damage = cancelled.max(Damage::Preview);
                } else {
                    effect.deactivate = true;
                }
            }
            Action::CommitText => effect.damage = self.commit_text(),
            Action::Backspace => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.damage = Damage::from_preview(edit.backspace());
                }
            }
            Action::BackspaceWord => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.damage = Damage::from_preview(edit.backspace_word());
                }
            }
            Action::MoveCursor(movement) => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.damage = Damage::from_preview(edit.move_cursor(movement));
                }
            }
            Action::InsertText(text) => {
                if let Some(edit) = self.text_edit_mut() {
                    edit.insert(&text);
                    effect.damage = Damage::Preview;
                }
            }
            _ => {}
        }
        effect.damage = effect.damage.max(Damage::from_preview(closed_picker));
        effect
    }

    pub fn open_picker(&mut self, center: Point) -> Damage {
        self.picker = Some(Picker {
            center,
            hovered: None,
        });
        Damage::Preview
    }

    pub fn picker_motion(&mut self, point: Point) -> Damage {
        let Some(picker) = &mut self.picker else {
            return Damage::None;
        };
        let choice = choice(picker.center, point, self.palette.len());
        let changed = picker.hovered != choice;
        picker.hovered = choice;
        Damage::from_preview(changed)
    }

    pub fn picker_release(&mut self, point: Point, latch_center: bool) -> Damage {
        let Some(picker) = self.picker else {
            return Damage::None;
        };
        let choice = choice(picker.center, point, self.palette.len());
        if choice.is_none() && latch_center {
            return Damage::None;
        }
        self.picker = None;
        match choice {
            Some(Choice::Color(index)) => Damage::Preview.max(self.apply_rgb(self.palette[index])),
            Some(Choice::Tool(tool)) => Damage::Preview.max(self.switch_tool(tool)),
            None => Damage::Preview,
        }
    }

    pub fn dismiss_picker(&mut self) -> Damage {
        Damage::from_preview(self.picker.take().is_some())
    }

    fn toggle_eraser(&mut self) -> Damage {
        let tool = if self.tool == Tool::Eraser {
            self.last_non_eraser_tool
        } else {
            Tool::Eraser
        };
        self.switch_tool(tool)
    }

    fn picker_tool(&self) -> Tool {
        if self.tool == Tool::Eraser {
            self.last_non_eraser_tool
        } else {
            self.tool
        }
    }

    pub fn append_preview_geometry(&self, output: &mut Vec<Geometry>) {
        match &self.interaction {
            Some(Interaction::Freehand(stroke)) => output.push(stroke.tail_geometry()),
            Some(Interaction::Drawing {
                tool,
                start,
                current,
                modifiers,
            }) => output.push(geometry(
                &drawing_kind(*tool, *start, *current, *modifiers),
                self.style,
            )),
            Some(Interaction::Moving {
                ids,
                start,
                current,
            }) => output.extend(ids.iter().filter_map(|id| {
                let delta = *current - *start;
                self.element(*id)
                    .map(|element| element.geometry.translated([delta.x, delta.y]))
            })),
            Some(Interaction::Resizing { id, current, .. }) => {
                if let Some(element) = self.element(*id) {
                    output.push(geometry(current, element.style));
                }
            }
            _ => {}
        }
    }

    pub fn append_selection_geometry(&self, show_handles: bool, output: &mut Vec<Geometry>) {
        if self.tool != Tool::Select {
            return;
        }
        if self.selected.len() > 1 {
            let mut bounds: Option<(Point, Point)> = None;
            for id in &self.selected {
                let Some(element) = self.element(*id) else {
                    continue;
                };
                let preview = match &self.interaction {
                    Some(Interaction::Moving {
                        ids,
                        start,
                        current,
                    }) if ids.contains(id) => Some(element.kind.translated(*current - *start)),
                    _ => None,
                };
                let kind = preview.as_ref().unwrap_or(&element.kind);
                let element_bounds = element.preview_bounds(kind);
                let (min, max) = (element_bounds.min, element_bounds.max);
                bounds = Some(bounds.map_or((min, max), |(current_min, current_max)| {
                    (
                        Point::new(current_min.x.min(min.x), current_min.y.min(min.y)),
                        Point::new(current_max.x.max(max.x), current_max.y.max(max.y)),
                    )
                }));
            }
            if let Some((min, max)) = bounds {
                output.push(selection::outline(min, max));
            }
            return;
        }
        if let Some(id) = self.selected.first() {
            self.append_selection_geometry_for(
                *id,
                show_handles && self.interaction.is_none(),
                output,
            );
        }
    }

    fn append_selection_geometry_for(
        &self,
        id: ElementId,
        show_handles: bool,
        output: &mut Vec<Geometry>,
    ) {
        let Some(element) = self.element(id) else {
            return;
        };
        let preview = match &self.interaction {
            Some(Interaction::Moving {
                ids,
                start,
                current,
            }) if ids.contains(&id) => Some(element.kind.translated(*current - *start)),
            Some(Interaction::Resizing {
                id: resizing_id,
                current,
                ..
            }) if *resizing_id == id => Some(current.clone()),
            _ => None,
        };
        let kind = preview.as_ref().unwrap_or(&element.kind);
        if !matches!(
            kind,
            ElementKind::Path { smooth: false, .. } | ElementKind::Triangle { .. }
        ) {
            let bounds = element.preview_bounds(kind);
            output.push(selection::outline(bounds.min, bounds.max));
        }
        if !show_handles {
            return;
        }
        selection::append_handles(kind, element.style, output);
    }

    pub fn picker_geometry(&self) -> Option<crate::render::LocalGeometry> {
        let picker = self.picker?;
        let active = self.picker_tool();
        Some(picker_geometry(
            picker.center,
            picker.hovered,
            active,
            self.current_color(),
            ShapeFills {
                triangle: self.tool_fill(Tool::Triangle),
                rectangle: self.tool_fill(Tool::Rectangle),
                ellipse: self.tool_fill(Tool::Ellipse),
            },
            &self.palette,
        ))
    }

    pub(super) fn active_text(&self) -> Option<&TextEdit> {
        self.text_edit()
    }

    pub fn element_is_previewed(&self, id: ElementId) -> bool {
        match &self.interaction {
            Some(Interaction::Moving { ids, .. }) => ids.contains(&id),
            Some(Interaction::Resizing { id: resized, .. }) => *resized == id,
            _ => false,
        }
    }

    pub fn moving_offset(&self, id: ElementId) -> Option<Point> {
        let Some(Interaction::Moving {
            ids,
            start,
            current,
        }) = &self.interaction
        else {
            return None;
        };
        ids.contains(&id).then_some(*current - *start)
    }

    fn switch_tool(&mut self, tool: Tool) -> Damage {
        if self.tool == tool {
            return Damage::None;
        }
        let damage = self.finish_interaction().max(Damage::Preview);
        self.selected.clear();
        self.tool = tool;
        if tool != Tool::Eraser {
            self.last_non_eraser_tool = tool;
        }
        self.sync_active_style();
        damage
    }
}

fn drawing_kind(tool: Tool, start: Point, current: Point, modifiers: Modifiers) -> ElementKind {
    match tool {
        Tool::Line | Tool::Arrow => ElementKind::Path {
            points: vec![
                start,
                selection::constrained_endpoint(start, current, modifiers.shift),
            ],
            smooth: false,
            end_marker: (tool == Tool::Arrow).then_some(EndMarker::Arrow),
        },
        Tool::Triangle => ElementKind::Triangle {
            vertices: selection::triangle_from_drag(start, current, modifiers),
        },
        Tool::Rectangle => {
            let (min, max) =
                selection::constrained_box(start, current, modifiers.shift, modifiers.alt);
            ElementKind::Rectangle { min, max }
        }
        Tool::Ellipse => {
            let (min, max) =
                selection::constrained_box(start, current, modifiers.shift, modifiers.alt);
            ElementKind::Ellipse {
                center: min.midpoint(max),
                radii: Point::new((max.x - min.x) * 0.5, (max.y - min.y) * 0.5),
            }
        }
        Tool::Pen | Tool::Text | Tool::Eraser | Tool::Select => unreachable!(),
    }
}
