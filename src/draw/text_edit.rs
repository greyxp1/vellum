use std::num::NonZeroUsize;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parley::{
    Affinity, Cursor, Layout, PlainEditor, PlainEditorDriver, Selection, SplitString, StyleProperty,
};
use unicode_segmentation::UnicodeSegmentation;

use super::decorations::text_caret;
use super::editor::Action;
use super::scene::{Bounds, ElementId, Point, Style, text_spec};
use crate::render::Geometry;
use crate::text::{TextFont, TextSpec, text_styles, with_text_context};

const HISTORY_LIMIT: usize = 256;
const GROUP_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct Snapshot {
    text: String,
    anchor: usize,
    focus: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Backspace,
    Delete,
    Atomic,
}

#[derive(Debug)]
struct EditGroup {
    kind: EditKind,
    cursor: usize,
    updated: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreeditHint {
    Whole,
    Selection,
    SpellingError,
    ComposeError,
    Prediction,
}

#[derive(Clone, Copy)]
pub(crate) enum CursorMove {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    TextStart,
    TextEnd,
}

impl CursorMove {
    /// Returns the Parley motions that move or extend the selection.
    fn motions<'a>(self) -> [fn(&mut PlainEditorDriver<'a, ()>); 2] {
        type Driver<'a> = PlainEditorDriver<'a, ()>;
        match self {
            Self::Left => [Driver::move_left, Driver::select_left],
            Self::Right => [Driver::move_right, Driver::select_right],
            Self::Up => [Driver::move_up, Driver::select_up],
            Self::Down => [Driver::move_down, Driver::select_down],
            Self::WordLeft => [Driver::move_word_left, Driver::select_word_left],
            Self::WordRight => [Driver::move_word_right, Driver::select_word_right],
            Self::Home => [Driver::move_to_line_start, Driver::select_to_line_start],
            Self::End => [Driver::move_to_line_end, Driver::select_to_line_end],
            Self::TextStart => [Driver::move_to_text_start, Driver::select_to_text_start],
            Self::TextEnd => [Driver::move_to_text_end, Driver::select_to_text_end],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreeditSpan {
    pub(crate) range: Range<usize>,
    pub(crate) style: PreeditHint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Preedit {
    pub(crate) text: String,
    pub(crate) cursor: Option<(usize, usize)>,
    pub(crate) spans: Vec<PreeditSpan>,
}

#[derive(Debug, Default)]
pub(crate) struct TextInputBatch {
    pub(crate) preedit: Option<Preedit>,
    pub(crate) commit: Option<String>,
    pub(crate) delete_surrounding: Option<(usize, usize)>,
    pub(crate) submit: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TextInputSnapshot<'a> {
    pub(crate) session: u64,
    pub(crate) generation: parley::Generation,
    pub(crate) content: SplitString<'a>,
    pub(crate) cursor: usize,
    pub(crate) anchor: usize,
    pub(crate) external_revision: u64,
    pub(crate) cursor_rectangle: Option<[i32; 4]>,
}

#[derive(Debug)]
pub(super) struct TextEdit {
    pub(super) session: u64,
    pub(super) id: Option<ElementId>,
    pub(super) origin: Point,
    editor: Box<PlainEditor<()>>,
    external_revision: u64,
    preedit_hints: Vec<PreeditSpan>,
    drag_point: Option<Point>,
    pub(super) style: Style,
    pub(super) scale: [f32; 2],
    pub(super) font: Arc<TextFont>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    group: Option<EditGroup>,
    composition_before: Option<Snapshot>,
}

impl TextEdit {
    pub(super) fn new(
        session: u64,
        id: Option<ElementId>,
        origin: Point,
        content: String,
        style: Style,
        scale: [f32; 2],
        font: Arc<TextFont>,
    ) -> Self {
        let mut editor = PlainEditor::new(style.size);
        editor.set_quantize(false);
        for property in text_styles(&font) {
            editor.edit_styles().insert(property);
        }
        editor.set_text(&content);
        with_text_context(|fonts, layouts| editor.driver(fonts, layouts).move_to_text_end());
        Self {
            session,
            id,
            origin,
            editor: Box::new(editor),
            external_revision: 0,
            preedit_hints: Vec::new(),
            drag_point: None,
            style,
            scale,
            font,
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
            composition_before: None,
        }
    }

    /// Accepts any composition, so committing never drops composed text.
    pub(super) fn finish(&self) -> String {
        self.editor.raw_text().to_owned()
    }

    pub(super) fn layout(&self) -> &Layout<()> {
        self.editor.try_layout().expect("text edits refresh layout")
    }

    pub(super) fn spec(&self) -> TextSpec<'_> {
        text_spec(self.layout(), self.origin, self.style, self.scale)
    }

    pub(super) fn bounds(&self) -> Bounds {
        self.spec().bounds().into()
    }

    fn local(&self, point: Point) -> (f32, f32) {
        (
            (point.x - self.origin.x) / self.scale[0],
            (point.y - self.origin.y) / self.scale[1],
        )
    }

    pub(super) fn click(&mut self, point: Point, clicks: u8, extend: bool) -> bool {
        self.drag_point = Some(point);
        let (x, y) = self.local(point);
        self.edit(None, true, |driver| match clicks {
            2 => driver.select_word_at_point(x, y),
            3 => driver.select_line_at_point(x, y),
            _ if extend => driver.shift_click_extension(x, y),
            _ => driver.move_to_point(x, y),
        })
    }

    pub(super) fn drag(&mut self, point: Point) -> bool {
        if self.drag_point.is_none_or(|previous| previous == point) {
            return false;
        }
        self.drag_point = Some(point);
        let (x, y) = self.local(point);
        self.edit(None, true, |driver| driver.extend_selection_to_point(x, y))
    }

    pub(super) fn end_drag(&mut self, point: Option<Point>) -> bool {
        let changed = point.is_some_and(|point| self.drag(point));
        self.drag_point = None;
        changed
    }

    pub(super) fn set_size(&mut self, size: f32) {
        self.group = None;
        self.style.size = size;
        self.editor
            .edit_styles()
            .insert(StyleProperty::FontSize(size));
        with_text_context(|fonts, layouts| self.editor.refresh_layout(fonts, layouts));
    }

    fn capture(&self) -> Snapshot {
        let selection = self.editor.raw_selection();
        Snapshot {
            text: self.editor.raw_text().to_owned(),
            anchor: selection.anchor().index(),
            focus: selection.focus().index(),
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        with_text_context(|fonts, layouts| {
            self.editor.driver(fonts, layouts).finish_compose();
            self.editor.set_text(&snapshot.text);
            self.editor
                .driver(fonts, layouts)
                .select_byte_range(snapshot.anchor, snapshot.focus);
        });
        self.preedit_hints.clear();
    }

    fn record(&mut self, before: Snapshot, kind: EditKind) {
        if before.text == self.editor.raw_text() {
            self.group = None;
            return;
        }
        let now = Instant::now();
        let selection = self.editor.raw_selection();
        let groupable = kind != EditKind::Atomic && before.anchor == before.focus;
        let groupable = groupable && selection.is_collapsed();
        let merge = groupable
            && self.group.as_ref().is_some_and(|group| {
                group.kind == kind
                    && group.cursor == before.focus
                    && now.duration_since(group.updated) <= GROUP_TIMEOUT
            });
        if !merge {
            if self.undo.len() == HISTORY_LIMIT {
                self.undo.remove(0);
            }
            self.undo.push(before);
        }
        self.redo.clear();
        self.group = groupable.then_some(EditGroup {
            kind,
            cursor: selection.focus().index(),
            updated: now,
        });
    }

    /// Runs one Parley edit, recording an undo step when it has a kind. Changes made outside of
    /// the input method are `external`, so it must then resynchronize.
    fn edit(
        &mut self,
        kind: Option<EditKind>,
        external: bool,
        edit: impl FnOnce(&mut PlainEditorDriver<'_, ()>),
    ) -> bool {
        let cancelled = self.clear_preedit();
        let before = kind.map(|kind| (self.capture(), kind));
        let generation = self.editor.generation();
        with_text_context(|fonts, layouts| edit(&mut self.editor.driver(fonts, layouts)));
        match before {
            Some((before, kind)) => self.record(before, kind),
            None => self.group = None,
        }
        let changed = cancelled || self.editor.generation() != generation;
        if changed && external {
            self.external_revision = self.external_revision.wrapping_add(1);
        }
        changed
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        self.editor.selected_text().map(str::to_owned)
    }

    /// Applies a text command, or returns None for actions the canvas handles.
    pub(super) fn apply(&mut self, action: &Action) -> Option<bool> {
        let insert = |text: &str| {
            if text.contains('\n') {
                EditKind::Atomic
            } else {
                EditKind::Insert
            }
        };
        Some(match action {
            Action::Undo => self.step(false),
            Action::Redo => self.step(true),
            Action::SelectAll => self.edit(None, true, |driver| driver.select_all()),
            Action::Backspace => self.delete_grapheme(true),
            Action::Delete => self.delete_grapheme(false),
            Action::BackspaceWord => self.edit(Some(EditKind::Atomic), true, |driver| {
                driver.backdelete_word()
            }),
            Action::MoveCursor(movement, extend) => self.move_cursor(*movement, *extend),
            Action::InsertText(text) => self.edit(Some(insert(text)), true, |driver| {
                driver.insert_or_replace_selection(text)
            }),
            Action::PasteText(text) => self.edit(Some(EditKind::Atomic), true, |driver| {
                driver.insert_or_replace_selection(text)
            }),
            Action::Cut if self.editor.selected_text().is_none() => false,
            Action::Cut => self.edit(Some(EditKind::Atomic), true, |driver| {
                driver.delete_selection()
            }),
            _ => return None,
        })
    }

    /// Exchanges the current text with the newest undo or redo snapshot.
    fn step(&mut self, redo: bool) -> bool {
        self.group = None;
        self.drag_point = None;
        if !self.clear_preedit() {
            let current = self.capture();
            let (from, to) = if redo {
                (&mut self.redo, &mut self.undo)
            } else {
                (&mut self.undo, &mut self.redo)
            };
            let Some(snapshot) = from.pop() else {
                return false;
            };
            to.push(current);
            self.restore(snapshot);
        }
        self.external_revision = self.external_revision.wrapping_add(1);
        true
    }

    fn delete_grapheme(&mut self, backwards: bool) -> bool {
        let kind = if backwards {
            EditKind::Backspace
        } else {
            EditKind::Delete
        };
        self.edit(Some(kind), true, |driver| {
            if !driver.editor.raw_selection().is_collapsed() {
                driver.delete_selection();
                return;
            }
            let cursor = driver.editor.raw_selection().focus().index();
            let text = driver.editor.raw_text();
            // Preserve whole-character deletion rather than shaped cluster/scalar deletion.
            let grapheme = if backwards {
                text[..cursor].graphemes(true).next_back()
            } else {
                text[cursor..].graphemes(true).next()
            };
            let Some(len) = NonZeroUsize::new(grapheme.map_or(0, str::len)) else {
                return;
            };
            if backwards {
                driver.delete_bytes_before_selection(len);
            } else {
                driver.delete_bytes_after_selection(len);
            }
        })
    }

    fn move_cursor(&mut self, movement: CursorMove, extend: bool) -> bool {
        self.edit(None, true, |driver| {
            let [to, select] = movement.motions();
            if extend { select(driver) } else { to(driver) }
        })
    }

    pub(super) fn clear_preedit(&mut self) -> bool {
        let changed = self.editor.is_composing();
        self.preedit_hints.clear();
        if let Some(before) = self.composition_before.take() {
            self.restore(before);
        } else if changed {
            with_text_context(|fonts, layouts| self.editor.driver(fonts, layouts).clear_compose());
        }
        if changed {
            self.group = None;
        }
        changed
    }

    pub(super) fn apply_text_input(&mut self, batch: TextInputBatch) -> bool {
        let generation = self.editor.generation();
        let changed = if batch.commit.is_some() || batch.delete_surrounding.is_some() {
            self.edit(Some(EditKind::Atomic), false, |driver| {
                if let Some((before, after)) = batch.delete_surrounding {
                    let selection = driver.editor.raw_selection().text_range();
                    let text = driver.editor.raw_text();
                    if before <= selection.start
                        && text.is_char_boundary(selection.start - before)
                        && text.is_char_boundary(selection.end.saturating_add(after))
                    {
                        if let Some(len) = NonZeroUsize::new(after) {
                            driver.delete_bytes_after_selection(len);
                        }
                        if let Some(len) = NonZeroUsize::new(before) {
                            driver.delete_bytes_before_selection(len);
                        }
                    }
                }
                if let Some(commit) = batch.commit {
                    driver.insert_or_replace_selection(&commit);
                }
            })
        } else {
            batch.preedit.is_none() && self.clear_preedit()
        };
        if let Some(preedit) = batch.preedit {
            if self.composition_before.is_none() {
                self.group = None;
                self.composition_before = Some(self.capture());
            }
            with_text_context(|fonts, layouts| {
                self.editor
                    .driver(fonts, layouts)
                    .set_compose(&preedit.text, preedit.cursor);
            });
            self.preedit_hints = preedit.spans;
        }
        changed || self.editor.generation() != generation
    }

    pub(super) fn caret(&self) -> Option<Geometry> {
        if !self.editor.raw_selection().is_collapsed() {
            return None;
        }
        let rect = self.editor.cursor_geometry(1.0)?;
        let [scale_x, scale_y] = self.scale;
        Some(text_caret(
            self.origin.x + rect.x0 as f32 * scale_x,
            self.origin.y + rect.y0 as f32 * scale_y,
            self.style.size * scale_y,
        ))
    }

    pub(super) fn ime_area(&self) -> parley::BoundingBox {
        self.editor.ime_cursor_area()
    }

    pub(super) fn decoration_rectangles(&self, mut draw: impl FnMut([f32; 4], PreeditHint)) {
        let layout = self.layout();
        let mut draw_selection = |selection: Selection, style| {
            selection.geometry_with(layout, |rect, _| {
                draw(
                    [
                        rect.x0 as f32,
                        rect.y0 as f32,
                        rect.width() as f32,
                        rect.height() as f32,
                    ],
                    style,
                );
            });
        };
        if let Some(range) = self.editor.raw_compose() {
            for (range, style) in std::iter::once((range.clone(), PreeditHint::Whole)).chain(
                self.preedit_hints.iter().map(|span| {
                    (
                        range.start + span.range.start..range.start + span.range.end,
                        span.style,
                    )
                }),
            ) {
                draw_selection(
                    Selection::new(
                        Cursor::from_byte_index(layout, range.start, Affinity::Downstream),
                        Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
                    ),
                    style,
                );
            }
        }
        draw_selection(*self.editor.raw_selection(), PreeditHint::Selection);
    }

    pub(super) fn snapshot(&self, cursor_rectangle: Option<[i32; 4]>) -> TextInputSnapshot<'_> {
        let selection = self.editor.raw_selection();
        let compose = self.editor.raw_compose().as_ref().map(|range| range.start);
        TextInputSnapshot {
            session: self.session,
            generation: self.editor.generation(),
            content: self.editor.text(),
            cursor: compose.unwrap_or_else(|| selection.focus().index()),
            anchor: compose.unwrap_or_else(|| selection.anchor().index()),
            external_revision: self.external_revision,
            cursor_rectangle,
        }
    }
}
