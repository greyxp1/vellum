mod elements;
mod interaction;
mod preview;
mod properties;

use self::interaction::Interaction;
pub(super) use self::properties::Property;
use self::properties::ToolPropertySet;
use super::Modifiers;
use super::history::{Entry as HistoryEntry, History};
use super::picker::{Choice, Picker, choice};
use super::scene::Element;
use super::scene::{ElementId, ElementKind, Point, Style};
use super::selection;
use super::text_edit::TextEdit;
use super::text_edit::{CursorMove, TextInputBatch};
use crate::tool::Tool;

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
    ApplyTextInput(TextInputBatch),
}

#[derive(Default)]
pub(crate) struct EditorEffect {
    pub changed: bool,
    pub deactivate: bool,
    pub feedback: Option<String>,
}

pub(super) struct Editor {
    tool: Tool,
    style: Style,
    // IDs increase on insertion; undo restores the original order.
    document: Document,
    other_document: Document,
    pub(in crate::draw) canvas: super::Canvas,
    selected: Vec<ElementId>,
    interaction: Option<Interaction>,
    next_id: ElementId,
    next_text_session: u64,
    picker: Option<Picker>,
    default_tool: Tool,
    default_style: Style,
    last_non_eraser_tool: Tool,
    tool_properties: ToolPropertySet,
    default_tool_properties: ToolPropertySet,
    size_ranges: std::sync::Arc<std::collections::BTreeMap<Tool, crate::config::SizeRange>>,
    steps: std::sync::Arc<std::collections::BTreeMap<Tool, crate::config::Steps>>,
    stops: std::sync::Arc<std::collections::BTreeMap<Tool, crate::config::Stops>>,
    remember_last_tool: bool,
    remember_tool_properties: bool,
    palette: Vec<[f32; 4]>,
    cursor_visibility: Option<bool>,
    tool_cursor_visibility: std::collections::BTreeMap<Tool, bool>,
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
        let default_tool_properties = ToolPropertySet::new(
            settings.stroke_size,
            settings.default_color[3],
            settings.default_fill_shapes,
            &settings.tool_defaults,
            &settings.size_ranges,
        );
        let tool_properties = default_tool_properties;
        let active = tool_properties.properties(settings.default_tool).copied();
        let fallback = tool_properties
            .properties(Tool::Pen)
            .copied()
            .expect("pen has adjustable properties");
        let active = active.unwrap_or(fallback);
        let mut color = settings.default_color;
        color[3] = active.opacity;
        let style = Style {
            size: active.size,
            color,
            roundness: active.roundness,
            filled: active.filled,
        };
        Self {
            tool: settings.default_tool,
            style,
            document: Document::default(),
            other_document: Document::default(),
            canvas: super::Canvas::Desktop,
            selected: Vec::new(),
            interaction: None,
            next_id: 1,
            next_text_session: 1,
            picker: None,
            default_tool: settings.default_tool,
            default_style: style,
            last_non_eraser_tool: if settings.default_tool == Tool::Eraser {
                Tool::Pen
            } else {
                settings.default_tool
            },
            tool_properties,
            default_tool_properties,
            size_ranges: settings.size_ranges.clone(),
            steps: settings.steps.clone(),
            stops: settings.stops.clone(),
            remember_last_tool: settings.remember_last_tool,
            remember_tool_properties: settings.remember_tool_properties,
            palette: settings.palette.clone(),
            cursor_visibility: settings.cursor.visible,
            adjusted: None,
            tool_cursor_visibility: settings
                .tool_defaults
                .iter()
                .filter_map(|(tool, defaults)| {
                    defaults.cursor.visible.map(|visible| (*tool, visible))
                })
                .collect(),
        }
    }

    pub(in crate::draw) fn reload(
        &mut self,
        settings: &crate::config::Settings,
        previous: &crate::config::Settings,
    ) {
        let next = Self::new(settings);
        self.tool_properties.reload(
            &self.default_tool_properties,
            &next.default_tool_properties,
            &settings.size_ranges,
        );
        self.default_tool_properties = next.default_tool_properties;
        self.default_style = next.default_style;
        self.size_ranges = next.size_ranges;
        self.steps = next.steps;
        self.stops = next.stops;
        self.remember_last_tool = next.remember_last_tool;
        self.remember_tool_properties = next.remember_tool_properties;
        self.palette = next.palette;
        self.cursor_visibility = next.cursor_visibility;
        self.tool_cursor_visibility = next.tool_cursor_visibility;
        self.picker = None;
        if settings.default_color[..3] != previous.default_color[..3] {
            self.style.color[..3].copy_from_slice(&settings.default_color[..3]);
        }
        if self.default_tool != settings.default_tool {
            self.switch_tool(settings.default_tool);
        }
        self.default_tool = settings.default_tool;
        self.sync_active_style();
        if settings.text_font != previous.text_font {
            for document in [&mut self.document, &mut self.other_document] {
                for element in &mut document.elements {
                    element.refresh_text_bounds();
                }
                document.history.refresh_text_bounds();
            }
        }
    }

    pub(in crate::draw) fn deactivate(&mut self) -> bool {
        let changed = self.finish_interaction();
        let clear_preview =
            !std::mem::take(&mut self.selected).is_empty() | self.picker.take().is_some();
        changed | clear_preview
    }

    pub(in crate::draw) fn reset_tools(&mut self) -> bool {
        if self.remember_last_tool && self.remember_tool_properties {
            return false;
        }
        if !self.remember_last_tool {
            self.tool = self.default_tool;
            self.last_non_eraser_tool = if self.default_tool == Tool::Eraser {
                Tool::Pen
            } else {
                self.default_tool
            };
        }
        if !self.remember_tool_properties {
            self.style = self.default_style;
            self.tool_properties = self.default_tool_properties;
        }
        self.sync_active_style();
        self.adjusted = None;
        true
    }

    pub(in crate::draw) fn is_editing_text(&self) -> bool {
        matches!(self.interaction, Some(Interaction::EditingText(_)))
    }

    pub(in crate::draw) fn is_drawing_pen(&self) -> bool {
        matches!(self.interaction, Some(Interaction::Freehand(_)))
    }

    pub(super) fn text_edit(&self) -> Option<&TextEdit> {
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

    pub(in crate::draw) fn current_color(&self) -> [f32; 4] {
        if let Some(edit) = self.text_edit() {
            edit.style.color
        } else if let Some(element) = self.selected.last().and_then(|id| self.element(*id)) {
            element.style.color
        } else {
            self.style.color
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
            Action::Undo if !self.is_editing_text() => effect.changed = self.undo(),
            Action::Redo if !self.is_editing_text() => effect.changed = self.redo(),
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
            Action::Undo | Action::Redo | Action::ApplyTextInput(_) => {}
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
        let choice = choice(picker.center, point, self.palette.len());
        let changed = picker.hovered != choice;
        picker.hovered = choice;
        changed
    }

    pub(in crate::draw) fn picker_release(&mut self, point: Point, latch_center: bool) -> bool {
        let Some(picker) = self.picker else {
            return false;
        };
        let choice = choice(picker.center, point, self.palette.len());
        if choice.is_none() && latch_center {
            return false;
        }
        self.picker = None;
        match choice {
            Some(Choice::Color(index)) => {
                self.apply_rgba(self.palette[index]);
            }
            Some(Choice::Tool(tool)) => {
                self.switch_tool(tool);
            }
            None => {}
        }
        true
    }

    pub(in crate::draw) fn dismiss_picker(&mut self) -> bool {
        self.picker.take().is_some()
    }

    fn toggle_eraser(&mut self) -> bool {
        let tool = if self.tool == Tool::Eraser {
            self.last_non_eraser_tool
        } else {
            Tool::Eraser
        };
        self.switch_tool(tool)
    }

    fn color_tool(&self) -> Tool {
        if self.tool == Tool::Eraser {
            self.last_non_eraser_tool
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
    ) -> TextEdit {
        let session = self.next_text_session;
        self.next_text_session = self.next_text_session.wrapping_add(1).max(1);
        TextEdit::new(session, id, origin, content, style, scale)
    }

    fn switch_tool(&mut self, tool: Tool) -> bool {
        if self.tool == tool {
            return false;
        }
        self.finish_interaction();
        self.selected.clear();
        self.tool = tool;
        if tool != Tool::Eraser {
            self.last_non_eraser_tool = tool;
        }
        self.sync_active_style();
        true
    }
}

fn drawing_kind(tool: Tool, start: Point, current: Point, modifiers: Modifiers) -> ElementKind {
    match tool {
        Tool::Line | Tool::Arrow => ElementKind::Segment {
            points: [
                start,
                selection::constrained_endpoint(start, current, modifiers.shift),
            ],
            arrow: tool == Tool::Arrow,
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
            ElementKind::ellipse(min, max)
        }
        Tool::Pen | Tool::Text | Tool::Eraser | Tool::Select => unreachable!(),
    }
}
