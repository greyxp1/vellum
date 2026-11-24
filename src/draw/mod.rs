//! Editing actions, transient feedback and per-output damage.

mod decorations;
mod editor;
mod freehand;
mod history;
mod picker;
mod presentation;
mod scene;
mod selection;
mod text_edit;
mod triangle;

use self::decorations::tool_cursor_geometry;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::OutputId;
use crate::config::Property;

pub(crate) use self::editor::Action;
use self::editor::{Editor, EditorEffect};
pub(crate) use self::scene::{Point, Style};
pub(crate) use self::selection::CursorHint;
use self::text_edit::TextEdit;
pub(crate) use self::text_edit::{
    CursorMove, Preedit, PreeditHint, PreeditSpan, TextInputBatch, TextInputSnapshot,
};
use crate::tool::Primitive;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ToolOverride {
    #[default]
    None,
    Eraser,
    InvertEraser,
}

impl ToolOverride {
    pub(crate) fn from_eraser(enabled: bool) -> Self {
        if enabled { Self::Eraser } else { Self::None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ToolCursor {
    pub tool: Primitive,
    pub size: f32,
    pub roundness: f32,
    pub color: crate::Rgba,
}

const CIRCLE_KAPPA: f64 = 0.552_284_749_830_793_6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Cursor {
    Hidden,
    Shape(CursorHint),
    Tool(ToolCursor),
}

impl Cursor {
    pub(crate) fn same_compositor_cursor(self, other: Self) -> bool {
        self.shape() == other.shape()
    }

    pub(crate) fn shape(self) -> Option<CursorHint> {
        match self {
            Self::Shape(hint) => Some(hint),
            Self::Hidden | Self::Tool(_) => None,
        }
    }

    pub(crate) fn tool(self) -> Option<ToolCursor> {
        match self {
            Self::Tool(preview) => Some(preview),
            Self::Hidden | Self::Shape(_) => None,
        }
    }
}

const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Canvas {
    Desktop,
    Whiteboard,
}

pub(crate) struct Presentation {
    pub canvas: Option<Canvas>,
    pub interactive: bool,
    pub background: Option<crate::Rgba>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

pub(crate) struct PenMotion {
    pub end: Point,
    pub bend: Option<Point>,
}

struct Feedback {
    text: crate::text::Text,
    anchor: Point,
    until: Instant,
}

struct OutputDamage {
    dirty: bool,
    viewport: Option<kurbo::Rect>,
}

pub(crate) struct DrawState {
    editor: Editor,
    outputs: BTreeMap<OutputId, OutputDamage>,
    feedback: Option<Feedback>,
    feedback_duration: Duration,
    caret_visible: bool,
    caret_until: Option<Instant>,
    tool_cursor: Option<(Point, ToolCursor)>,
    fade_frame: Option<Instant>,
    shadows: bool,
    shadow_overlap: bool,
    // Changes with committed annotations so outputs can reuse their last rendering.
    committed_revision: u64,
}

impl DrawState {
    pub(crate) fn select_tool(&mut self, tool: &str) -> Result<bool, String> {
        let was_editing_text = self.editor.is_editing_text();
        let changed = self.editor.switch_named_tool(tool)?;
        Ok(if was_editing_text {
            self.record(changed)
        } else {
            self.record_overlay(changed)
        })
    }

    pub(crate) fn set_tool_setting(
        &mut self,
        tool: Option<&str>,
        setting: crate::cli::ToolSetting,
    ) -> Result<bool, String> {
        let changed = self.editor.set_tool_setting(tool, setting)?;
        self.record_overlay(changed && self.editor.picker_active());
        Ok(changed)
    }

    pub(crate) fn tool_name(&self) -> &str {
        self.editor.tool_name()
    }

    pub(crate) fn named_tool_style(
        &self,
        tool: Option<&str>,
    ) -> Result<(Primitive, Style), String> {
        self.editor.named_tool_style(tool)
    }

    pub(crate) fn switch_canvas(&mut self, canvas: Canvas) {
        self.editor.switch_canvas(canvas);
        self.record(true);
    }

    pub(crate) fn is_presenting(&self) -> bool {
        self.editor.presentation
    }

    pub(crate) fn set_presentation(&mut self, enabled: bool) {
        let changed = self.editor.presentation != enabled;
        self.editor.presentation = enabled;
        self.record(changed);
    }

    pub(crate) fn clear_whiteboard(&mut self) -> bool {
        let changed = self.editor.clear_whiteboard();
        self.record(changed)
    }

    pub(super) fn new(settings: &crate::config::Settings) -> Self {
        let feedback_duration = settings.feedback_duration;
        let editor = Editor::new(settings);
        Self {
            editor,
            outputs: BTreeMap::new(),
            feedback: None,
            feedback_duration,
            caret_visible: true,
            caret_until: None,
            tool_cursor: None,
            fade_frame: None,
            shadows: settings.annotation_shadows,
            shadow_overlap: settings.annotation_shadow_overlap,
            committed_revision: 0,
        }
    }

    pub(crate) fn reload(&mut self, settings: &crate::config::Settings) {
        self.editor.reload(settings);
        self.feedback_duration = settings.feedback_duration;
        self.shadows = settings.annotation_shadows;
        self.shadow_overlap = settings.annotation_shadow_overlap;
        self.feedback = None;
        self.tool_cursor = None;
        self.record(true);
    }

    pub(crate) fn deactivate(&mut self, preserve_picker: bool) -> bool {
        let mut changed = self.editor.deactivate(preserve_picker);
        if self.feedback.take().is_some() | self.tool_cursor.take().is_some() {
            changed = true;
        }
        self.caret_until = None;
        self.record(changed)
    }

    pub(crate) fn reset_tools(&mut self) {
        let changed = self.editor.reset_tools();
        self.record(changed);
    }

    pub(crate) fn is_editing_text(&self) -> bool {
        self.editor.is_editing_text()
    }

    pub(crate) fn is_drawing_pen(&self) -> bool {
        self.editor.is_drawing_pen()
    }

    pub(crate) fn handle_action(&mut self, action: Action, at: Option<Point>) -> EditorEffect {
        let user_input = !matches!(action, Action::ApplyTextInput(_));
        let mut effect = self.editor.handle_action(action);
        if let (Some(label), Some(at)) = (effect.feedback.take(), at) {
            self.feedback = Some(Feedback {
                text: crate::text::Text::new(label, self.editor.text_font(), 18.0),
                anchor: at,
                until: Instant::now() + self.feedback_duration,
            });
            effect.changed = true;
        }
        if (user_input || effect.changed) && self.show_caret() {
            effect.changed = true;
        }
        self.record(effect.changed);
        effect
    }

    pub(crate) fn pointer_down(
        &mut self,
        point: Point,
        modifiers: Modifiers,
        tool_override: ToolOverride,
    ) -> bool {
        let before = self.editor.interaction_bounds();
        let was_editing_text = self.editor.text_edit().is_some();
        let mut changed = self.editor.pointer_down(point, modifiers, tool_override);
        if self.show_caret() {
            changed = true;
        }
        if !was_editing_text && self.editor.has_preview() {
            self.damage_interaction(changed, before)
        } else {
            self.record(changed)
        }
    }

    pub(crate) fn pointer_motion(&mut self, point: Point, modifiers: Modifiers) -> bool {
        // Erasing edits the document while other gestures only change previews.
        let erasing = self.editor.is_erasing();
        let before = self.editor.interaction_bounds();
        let changed = self.editor.pointer_motion(point, modifiers);
        if changed {
            self.show_caret();
        }
        if erasing {
            self.record(changed)
        } else {
            self.damage_interaction(changed, before)
        }
    }

    pub(crate) fn pen_motion(&mut self, motion: PenMotion, modifiers: Modifiers) -> bool {
        let before = self.editor.interaction_bounds();
        let changed = self.editor.pen_motion(motion, modifiers);
        self.damage_interaction(changed, before)
    }

    pub(crate) fn pen_release_tip(&self, point: Point, modifiers: Modifiers) -> Option<Point> {
        self.editor.pen_release_tip(point, modifiers)
    }

    pub(crate) fn modifiers_changed(&mut self, modifiers: Modifiers) -> bool {
        let before = self.editor.interaction_bounds();
        let changed = self.editor.modifiers_changed(modifiers);
        self.damage_interaction(changed, before)
    }

    pub(crate) fn pointer_up(&mut self, point: Point, modifiers: Modifiers) -> bool {
        let changed = self.editor.pointer_up(point, modifiers);
        if changed {
            self.show_caret();
        }
        self.record(changed)
    }

    pub(crate) fn picker_active(&self) -> bool {
        self.editor.picker_active()
    }

    pub(crate) fn cursor(&self, point: Point, tool_override: ToolOverride) -> Cursor {
        self.editor.cursor(point, tool_override)
    }

    pub(crate) fn finish_pointer_interaction(
        &mut self,
        point: Option<Point>,
        modifiers: Modifiers,
    ) -> bool {
        let changed = self.editor.finish_pointer_interaction(point, modifiers);
        if changed {
            self.show_caret();
        }
        self.record(changed)
    }

    pub(crate) fn set_tool_cursor(&mut self, cursor: Option<(Point, ToolCursor)>) -> bool {
        if self.tool_cursor == cursor {
            return false;
        }
        for (point, cursor) in [self.tool_cursor, cursor].into_iter().flatten() {
            if let Some(bounds) = tool_cursor_geometry(point, cursor).bounds() {
                let margin = if self.shadows { 10.0 } else { 0.0 };
                self.damage_region(bounds.inflate(margin, margin));
            }
        }
        self.tool_cursor = cursor;
        true
    }

    pub(crate) fn open_picker(&mut self, center: Point) {
        let before = self.editor.interaction_bounds();
        self.editor.open_picker(center);
        self.damage_interaction(true, before);
    }

    pub(crate) fn picker_motion(&mut self, point: Point) -> bool {
        let before = self.editor.interaction_bounds();
        let changed = self.editor.picker_motion(point);
        self.damage_interaction(changed, before)
    }

    pub(crate) fn picker_release(&mut self, point: Point, latch_center: bool) -> bool {
        let changed = self.editor.picker_release(point, latch_center);
        self.record(changed)
    }

    pub(crate) fn dismiss_picker(&mut self) -> bool {
        let before = self.editor.interaction_bounds();
        let changed = self.editor.dismiss_picker();
        self.damage_interaction(changed, before)
    }

    pub(crate) fn text_click_at(&mut self, point: Point, clicks: u8) -> Option<bool> {
        let mut changed = self.editor.text_click_at(point, clicks)?;
        changed |= self.show_caret();
        Some(self.record(changed))
    }

    pub(crate) fn adjust(&mut self, steps: f32, at: Point, modifiers: Modifiers) -> bool {
        let adjustment = if modifiers.shift {
            self.editor.adjust(Property::Roundness, steps)
        } else if modifiers.ctrl {
            self.editor.adjust(Property::Opacity, steps)
        } else {
            self.editor.adjust(Property::Size, steps)
        };
        if adjustment.changed {
            self.record(true);
        } else if adjustment.feedback.is_some() {
            self.record_overlay(true);
        }
        if let Some(text) = adjustment.feedback {
            self.feedback = Some(Feedback {
                text: crate::text::Text::new(text, self.editor.text_font(), 18.0),
                anchor: at,
                until: Instant::now() + self.feedback_duration,
            });
        }
        adjustment.hit_stop
    }

    pub(crate) fn add_output(&mut self, output: OutputId) {
        self.outputs.insert(
            output,
            OutputDamage {
                dirty: true,
                viewport: None,
            },
        );
    }

    pub(crate) fn remove_output(&mut self, output: OutputId) {
        self.outputs.remove(&output);
    }

    pub(crate) fn text_input_snapshot(&self) -> Option<TextInputSnapshot<'_>> {
        self.editor.text_edit().map(|edit| edit.snapshot(None))
    }

    pub(crate) fn clipboard_text(&self) -> Option<String> {
        self.editor.text_edit().and_then(TextEdit::selected_text)
    }

    pub(crate) fn clear_preedit(&mut self) -> bool {
        if self.editor.clear_preedit() {
            self.show_caret();
            self.record(true)
        } else {
            false
        }
    }

    pub(crate) fn needs_render(&self, output: OutputId) -> bool {
        self.outputs.get(&output).is_some_and(|output| output.dirty)
    }

    pub(crate) fn damaged_outputs(&self) -> impl Iterator<Item = OutputId> + '_ {
        self.outputs
            .iter()
            .filter(|(_, damage)| damage.dirty)
            .map(|(&output, _)| output)
    }

    pub(crate) fn damage(&mut self, output: OutputId) {
        if let Some(output) = self.outputs.get_mut(&output) {
            output.dirty = true;
            // Presentation changes such as visibility or canvas selection bypass `record`.
            self.committed_revision += 1;
        }
    }

    fn damage_region(&mut self, bounds: kurbo::Rect) {
        let bounds = bounds.inflate(1.0, 1.0);
        for output in self.outputs.values_mut() {
            if output
                .viewport
                .is_none_or(|viewport| viewport.intersect(bounds).area() > 0.0)
            {
                output.dirty = true;
            }
        }
    }

    // Damages the outputs under a live gesture or picker before and after an uncommitted change.
    fn damage_interaction(&mut self, changed: bool, before: Option<kurbo::Rect>) -> bool {
        if !changed {
            return false;
        }
        let after = self.editor.interaction_bounds();
        if before.is_none() && after.is_none() {
            return self.record(true);
        }
        let margin = if self.shadows { 10.0 } else { 1.0 };
        for bounds in [before, after].into_iter().flatten() {
            self.damage_region(bounds.inflate(margin, margin));
        }
        true
    }

    fn record(&mut self, changed: bool) -> bool {
        if changed {
            self.committed_revision += 1;
            self.fade_frame = self
                .editor
                .next_fade(Instant::now())
                .map(|next| self.fade_frame.map_or(next, |current| current.min(next)));
            for output in self.outputs.values_mut() {
                output.dirty = true;
            }
        }
        changed
    }

    fn record_overlay(&mut self, changed: bool) -> bool {
        if changed {
            for output in self.outputs.values_mut() {
                output.dirty = true;
            }
        }
        changed
    }

    pub(crate) fn next_wakeup(&self) -> Option<Instant> {
        [
            self.feedback.as_ref().map(|feedback| feedback.until),
            self.caret_until,
            self.fade_frame,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn handle_timeouts(&mut self, now: Instant) -> bool {
        let fading = self.fade_frame.is_some_and(|until| now >= until);
        if fading {
            self.fade_frame = None;
            self.editor.expire_annotations(now);
        }
        let mut changed = fading;
        let mut feedback_expired = false;
        if self
            .feedback
            .as_ref()
            .is_some_and(|feedback| now >= feedback.until)
        {
            self.feedback = None;
            feedback_expired = true;
            changed = true;
        }
        if self.caret_until.is_some_and(|until| now >= until) {
            if let Some(edit) = self.editor.text_edit() {
                if let Some(bounds) = edit.caret().bounds() {
                    self.damage_region(bounds);
                }
                self.caret_visible = !self.caret_visible;
                self.caret_until = Some(now + CARET_BLINK_INTERVAL);
                changed = true;
            } else {
                self.caret_until = None;
            }
        }
        if fading {
            self.record(true);
        } else if feedback_expired {
            self.record_overlay(true);
        }
        changed
    }

    fn show_caret(&mut self) -> bool {
        if !self.editor.text_edit().is_some_and(TextEdit::shows_caret) {
            self.caret_until = None;
            return false;
        }
        let changed = !self.caret_visible;
        self.caret_visible = true;
        self.caret_until = Some(Instant::now() + CARET_BLINK_INTERVAL);
        changed
    }
}
