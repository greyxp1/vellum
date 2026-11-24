mod elements;
mod interaction;
mod preview;
mod properties;

use self::interaction::Interaction;
use super::Modifiers;
use super::history::{Entry as HistoryEntry, History};
use super::picker::{Choice, Picker, choice};
use super::scene::Element;
use super::scene::{ElementId, ElementKind, Point, Style};
use super::selection;
use super::text_edit::TextEdit;
use super::text_edit::{CursorMove, TextInputBatch};
use crate::Rgba;
use crate::config::{Property, ToolDefinition, ToolProperties};
use crate::tool::Primitive;
use crate::tool::ToolId;
use std::sync::Arc;

#[derive(Default)]
struct Document {
    elements: Vec<Element>,
    history: History,
}

impl Document {
    fn element_index(&self, id: ElementId) -> Option<usize> {
        self.elements
            .binary_search_by_key(&id, |element| element.id)
            .ok()
    }

    fn clear(&mut self) -> bool {
        if self.elements.is_empty() {
            return false;
        }
        self.history
            .record(HistoryEntry::Clear(std::mem::take(&mut self.elements)));
        true
    }
}

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
    MoveCursor(CursorMove, bool),
    InsertText(String),
    Copy,
    Cut,
    Paste,
    PasteText(String),
    ApplyTextInput(TextInputBatch),
}

#[derive(Default)]
pub(crate) struct EditorEffect {
    pub changed: bool,
    pub deactivate: bool,
    pub feedback: Option<String>,
}

pub(super) struct Editor {
    tool: ToolId,
    tools: Arc<[ToolDefinition]>,
    tool_properties: Vec<ToolProperties>,
    color: [f32; 3],
    default_color: [f32; 3],
    tool_wheel: Vec<ToolId>,
    // IDs increase on insertion; undo restores the original order.
    document: Document,
    other_document: Document,
    pub(in crate::draw) canvas: super::Canvas,
    pub(in crate::draw) presentation: bool,
    pub(in crate::draw) presentation_settings: crate::config::PresentationSettings,
    selected: Vec<ElementId>,
    interaction: Option<Interaction>,
    next_id: ElementId,
    next_text_session: u64,
    picker: Option<Picker>,
    default_tool: ToolId,
    last_non_eraser_tool: Option<ToolId>,
    remember_last_tool: bool,
    remember_tool_properties: bool,
    palette: Vec<Rgba>,
    // Wheel adjustments of one property merge into one undo step.
    adjusted: Option<Property>,
}

impl Editor {
    pub(in crate::draw) fn switch_canvas(&mut self, canvas: super::Canvas) {
        if self.canvas != canvas {
            std::mem::swap(&mut self.document, &mut self.other_document);
            self.canvas = canvas;
        }
    }

    pub(in crate::draw) fn canvas_elements(&self, canvas: super::Canvas) -> &[Element] {
        if self.canvas == canvas {
            &self.document.elements
        } else {
            &self.other_document.elements
        }
    }

    pub(in crate::draw) fn clear_whiteboard(&mut self) -> bool {
        if self.canvas == super::Canvas::Whiteboard {
            self.clear()
        } else {
            self.other_document.clear()
        }
    }

    pub(in crate::draw) fn new(settings: &crate::config::Settings) -> Self {
        let tool = settings.default_tool;
        let last_non_eraser_tool =
            (settings.tools[tool.0].primitive != Primitive::Eraser).then_some(tool);
        let [r, g, b, _] = settings.default_color;
        Self {
            tool,
            tools: settings.tools.clone(),
            tool_properties: settings.tools.iter().map(|tool| tool.properties).collect(),
            color: [r, g, b],
            default_color: [r, g, b],
            tool_wheel: settings.tool_wheel.clone(),
            document: Document::default(),
            other_document: Document::default(),
            canvas: super::Canvas::Desktop,
            presentation: false,
            presentation_settings: settings.presentation,
            selected: Vec::new(),
            interaction: None,
            next_id: 1,
            next_text_session: 1,
            picker: None,
            default_tool: tool,
            last_non_eraser_tool,
            remember_last_tool: settings.remember_last_tool,
            remember_tool_properties: settings.remember_tool_properties,
            palette: settings.palette.clone(),
            adjusted: None,
        }
    }

    pub(in crate::draw) fn reload(&mut self, settings: &crate::config::Settings) {
        let next = &settings.tools;
        let default_changed =
            self.tools[self.default_tool.0].id != next[settings.default_tool.0].id;
        let active = if default_changed {
            settings.default_tool
        } else {
            ToolDefinition::find(next, self.tool_name()).unwrap_or(settings.default_tool)
        };
        let tool_changed = next[active.0].id.as_ref() != self.tool_name()
            || next[active.0].primitive != self.primitive();
        if tool_changed || self.interaction.is_some() {
            self.finish_interaction();
        }
        if tool_changed {
            self.selected.clear();
        }
        self.last_non_eraser_tool = self
            .last_non_eraser_tool
            .map(|id| self.tools[id.0].id.as_ref())
            .and_then(|name| ToolDefinition::find(next, name))
            .filter(|id| next[id.0].primitive != Primitive::Eraser);
        self.tool_properties = next
            .iter()
            .map(|tool| {
                let mut properties = tool.properties;
                if let Some(id) = ToolDefinition::find(&self.tools, &tool.id)
                    && self.tools[id.0].primitive == tool.primitive
                {
                    let defaults = self.tools[id.0].properties;
                    let old = self.tool_properties[id.0];
                    if defaults.size == properties.size {
                        properties.size = tool.size_range.clamp(old.size);
                    }
                    if defaults.opacity == properties.opacity {
                        properties.opacity = old.opacity;
                    }
                    if defaults.roundness == properties.roundness {
                        properties.roundness = old.roundness;
                    }
                    if defaults.filled == properties.filled {
                        properties.filled = old.filled;
                    }
                }
                properties
            })
            .collect();
        self.tools = next.clone();
        self.tool = active;
        self.default_tool = settings.default_tool;
        let [r, g, b, _] = settings.default_color;
        if self.default_color != [r, g, b] {
            self.default_color = [r, g, b];
            self.color = self.default_color;
        }
        if self.primitive() != Primitive::Eraser {
            self.last_non_eraser_tool = Some(self.tool);
        }
        self.tool_wheel = settings.tool_wheel.clone();
        self.presentation_settings = settings.presentation;
        self.remember_last_tool = settings.remember_last_tool;
        self.remember_tool_properties = settings.remember_tool_properties;
        self.palette = settings.palette.clone();
        self.picker = None;
    }

    pub(in crate::draw) fn deactivate(&mut self, preserve_picker: bool) -> bool {
        let changed = self.finish_interaction();
        let clear_preview = !std::mem::take(&mut self.selected).is_empty()
            | (!preserve_picker && self.picker.take().is_some());
        changed | clear_preview
    }

    pub(in crate::draw) fn reset_tools(&mut self) -> bool {
        if self.remember_last_tool && self.remember_tool_properties {
            return false;
        }
        if !self.remember_last_tool {
            self.tool = self.default_tool;
            self.last_non_eraser_tool = None;
        }
        if !self.remember_tool_properties {
            self.color = self.default_color;
            for (properties, tool) in self.tool_properties.iter_mut().zip(self.tools.iter()) {
                *properties = tool.properties;
            }
        }
        if self.primitive() != Primitive::Eraser {
            self.last_non_eraser_tool = Some(self.tool);
        }
        self.adjusted = None;
        true
    }

    pub(in crate::draw) fn is_editing_text(&self) -> bool {
        matches!(self.interaction, Some(Interaction::EditingText(..)))
    }

    pub(in crate::draw) fn is_erasing(&self) -> bool {
        matches!(self.interaction, Some(Interaction::Erasing { .. }))
    }

    pub(in crate::draw) fn is_drawing_pen(&self) -> bool {
        matches!(self.interaction, Some(Interaction::Freehand(..)))
    }

    pub(super) fn text_edit(&self) -> Option<&TextEdit> {
        match &self.interaction {
            Some(Interaction::EditingText(_, edit)) => Some(edit),
            _ => None,
        }
    }

    fn text_edit_mut(&mut self) -> Option<&mut TextEdit> {
        match &mut self.interaction {
            Some(Interaction::EditingText(_, edit)) => Some(edit),
            _ => None,
        }
    }

    pub(in crate::draw) fn current_color(&self) -> Rgba {
        if let Some(edit) = self.text_edit() {
            edit.style.color
        } else if let Some(element) = self.selected.last().and_then(|id| self.element(*id)) {
            element.style.color
        } else {
            self.tool_style(self.color_tool()).color
        }
    }

    pub(in crate::draw) fn picker_active(&self) -> bool {
        self.picker.is_some()
    }

    pub(in crate::draw) fn handle_action(&mut self, action: Action) -> EditorEffect {
        let mut effect = EditorEffect::default();
        if let Action::ApplyTextInput(batch) = action {
            let submit = batch.submit;
            if let Some(edit) = self.text_edit_mut() {
                effect.changed = edit.apply_text_input(batch);
                if submit {
                    effect.changed |= self.commit_text();
                }
            }
            return effect;
        }
        let closed_picker = self.picker.take().is_some();
        if closed_picker && matches!(action, Action::Cancel) {
            effect.changed = true;
            return effect;
        }
        match action {
            Action::Undo => {
                effect.changed = if let Some(edit) = self.text_edit_mut() {
                    edit.undo()
                } else {
                    self.undo()
                };
            }
            Action::Redo => {
                effect.changed = if let Some(edit) = self.text_edit_mut() {
                    edit.redo()
                } else {
                    self.redo()
                };
            }
            Action::SelectAll => {
                effect.changed = if let Some(edit) = self.text_edit_mut() {
                    edit.select_all()
                } else {
                    self.select_all()
                };
            }
            Action::ToggleEraser => effect.changed = self.toggle_eraser(),
            Action::ToggleFill => {
                let adjustment = self.toggle_fill();
                effect.changed = adjustment.changed;
                effect.feedback = adjustment.feedback;
            }
            Action::Delete => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.changed = edit.delete();
                } else {
                    effect.changed = self.delete_selection();
                }
            }
            Action::Clear => effect.changed = self.clear(),
            Action::Cancel => {
                let cancelled = self.cancel_interaction();
                if cancelled || !std::mem::take(&mut self.selected).is_empty() {
                    effect.changed = true;
                } else {
                    effect.deactivate = true;
                }
            }
            Action::CommitText => effect.changed = self.commit_text(),
            Action::Backspace => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.changed = edit.backspace();
                }
            }
            Action::BackspaceWord => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.changed = edit.backspace_word();
                }
            }
            Action::MoveCursor(movement, extend) => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.changed = edit.move_cursor(movement, extend);
                }
            }
            Action::InsertText(text) => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.changed = edit.insert(&text);
                }
            }
            Action::Cut => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.changed = edit.cut();
                }
            }
            Action::PasteText(text) => {
                if let Some(edit) = self.text_edit_mut() {
                    effect.changed = edit.paste(&text);
                }
            }
            Action::Copy | Action::Paste | Action::ApplyTextInput(_) => {}
        }
        effect.changed |= closed_picker;
        effect
    }

    pub(in crate::draw) fn open_picker(&mut self, center: Point) {
        self.picker = Some(Picker {
            center,
            hovered: None,
        });
    }

    pub(in crate::draw) fn picker_motion(&mut self, point: Point) -> bool {
        let Some(picker) = &mut self.picker else {
            return false;
        };
        let choice = choice(
            picker.center,
            point,
            self.palette.len(),
            self.tool_wheel.len(),
        );
        let changed = picker.hovered != choice;
        picker.hovered = choice;
        changed
    }

    pub(in crate::draw) fn picker_release(&mut self, point: Point, latch_center: bool) -> bool {
        let Some(picker) = self.picker else {
            return false;
        };
        let choice = choice(
            picker.center,
            point,
            self.palette.len(),
            self.tool_wheel.len(),
        );
        if choice.is_none() && latch_center {
            return false;
        }
        self.picker = None;
        match choice {
            Some(Choice::Color(index)) => {
                self.apply_color(self.palette[index]);
            }
            Some(Choice::Tool(index)) => {
                let id = self.tool_wheel[index];
                self.switch_tool_id(id);
            }
            None => {}
        }
        true
    }

    pub(in crate::draw) fn dismiss_picker(&mut self) -> bool {
        self.picker.take().is_some()
    }

    fn toggle_eraser(&mut self) -> bool {
        let next = self.effective_tool(super::ToolOverride::InvertEraser);
        self.switch_tool_id(next)
    }

    fn color_tool(&self) -> ToolId {
        if self.primitive() == Primitive::Eraser {
            self.last_non_eraser_tool
                .filter(|id| self.tools[id.0].primitive.supports_opacity())
                .unwrap_or_else(|| self.builtin(Primitive::Pen))
        } else {
            self.tool
        }
    }

    pub(super) fn clear_preedit(&mut self) -> bool {
        self.text_edit_mut().is_some_and(TextEdit::clear_preedit)
    }

    fn make_text_edit(
        &mut self,
        id: Option<ElementId>,
        origin: Point,
        content: String,
        style: Style,
        scale: [f32; 2],
        font: std::sync::Arc<crate::text::TextFont>,
    ) -> TextEdit {
        let session = self.next_text_session;
        self.next_text_session = self.next_text_session.wrapping_add(1).max(1);
        TextEdit::new(session, id, origin, content, style, scale, font)
    }

    pub(in crate::draw) fn switch_tool(&mut self, primitive: Primitive) -> bool {
        self.switch_tool_id(self.builtin(primitive))
    }

    fn builtin(&self, primitive: Primitive) -> ToolId {
        ToolDefinition::builtin(&self.tools, primitive)
    }

    fn tool_style(&self, id: ToolId) -> Style {
        self.tool_properties[id.0].style(self.color)
    }

    fn primitive(&self) -> Primitive {
        self.tools[self.tool.0].primitive
    }

    fn effective_tool(&self, tool_override: super::ToolOverride) -> ToolId {
        match tool_override {
            super::ToolOverride::None => self.tool,
            super::ToolOverride::InvertEraser if self.primitive() == Primitive::Eraser => self
                .last_non_eraser_tool
                .unwrap_or_else(|| self.builtin(Primitive::Pen)),
            super::ToolOverride::Eraser | super::ToolOverride::InvertEraser => {
                self.builtin(Primitive::Eraser)
            }
        }
    }

    pub(in crate::draw) fn tool_name(&self) -> &str {
        &self.tools[self.tool.0].id
    }

    pub(in crate::draw) fn resolve_tool(&self, id: &str) -> Result<ToolId, String> {
        ToolDefinition::find(&self.tools, id).ok_or_else(|| format!("unknown tool {id:?}"))
    }

    pub(in crate::draw) fn switch_named_tool(&mut self, id: &str) -> Result<bool, String> {
        let id = self.resolve_tool(id)?;
        Ok(self.switch_tool_id(id))
    }

    fn switch_tool_id(&mut self, id: ToolId) -> bool {
        if self.tool == id {
            return false;
        }
        self.picker = None;
        self.finish_interaction();
        self.selected.clear();
        self.tool = id;
        if self.primitive() != Primitive::Eraser {
            self.last_non_eraser_tool = Some(id);
        }
        true
    }

    fn tool_font(&self, id: ToolId) -> Arc<crate::text::TextFont> {
        self.tools[id.0]
            .font
            .as_ref()
            .expect("text tools have fonts")
            .clone()
    }

    pub(in crate::draw) fn text_font(&self) -> Arc<crate::text::TextFont> {
        self.tool_font(self.builtin(Primitive::Text))
    }
}

fn drawing_kind(
    tool: Primitive,
    start: Point,
    current: Point,
    modifiers: Modifiers,
) -> ElementKind {
    match tool {
        Primitive::Line | Primitive::Arrow => ElementKind::Segment {
            points: [
                start,
                selection::constrained_endpoint(start, current, modifiers.shift),
            ],
            arrow: tool == Primitive::Arrow,
        },
        Primitive::Triangle => ElementKind::Triangle {
            vertices: selection::triangle_from_drag(start, current, modifiers),
        },
        Primitive::Rectangle => {
            let (min, max) =
                selection::constrained_box(start, current, modifiers.shift, modifiers.alt);
            ElementKind::Rectangle { min, max }
        }
        Primitive::Ellipse => {
            let (min, max) =
                selection::constrained_box(start, current, modifiers.shift, modifiers.alt);
            ElementKind::ellipse(min, max)
        }
        Primitive::Pen | Primitive::Text | Primitive::Eraser | Primitive::Select => unreachable!(),
    }
}
