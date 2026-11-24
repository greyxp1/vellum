use super::{Editor, ToolDefinition, drawing_kind};
use crate::draw::scene::{Bounds, ElementId, ElementKind, Point, Style, bounds_for};
use crate::draw::selection::{self, CursorHint, Handle};
use crate::draw::text_edit::TextEdit;
use crate::draw::{Cursor, Modifiers, PenMotion, ToolCursor, ToolOverride, freehand};
use crate::text::text_line_height;
use crate::tool::Primitive;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub(in crate::draw) struct ResizeSnapshot {
    pub(in crate::draw) kind: ElementKind,
    pub(in crate::draw) style: Style,
    pub(in crate::draw) bounds: Bounds,
}

impl ResizeSnapshot {
    fn set_style(&mut self, style: Style) {
        self.kind.prepare(style);
        if !matches!(self.kind, ElementKind::Text { .. })
            || style.size != self.style.size
            || style.filled != self.style.filled
            || (style.filled && style.roundness != self.style.roundness)
        {
            self.bounds = bounds_for(&self.kind, style);
        }
        self.style = style;
    }
}

#[derive(Debug)]
pub(super) struct Resize {
    pub(super) id: ElementId,
    pub(super) preset: Arc<str>,
    pub(super) handle: Handle,
    pub(super) current: ResizeSnapshot,
    text_size_range: [f32; 2],
    start: Point,
    point: Point,
    original: ResizeSnapshot,
    /// The resized element's style without font size changes made by resizing text.
    properties: Style,
    equal_side_anchor: Option<usize>,
}

impl Resize {
    /// Recomputes the preview for a pointer position, returning whether it changed.
    fn update(&mut self, point: Point, modifiers: Modifiers) -> bool {
        self.point = point;
        let delta = point - self.start;
        let original = &self.original;
        let resized = if matches!(original.kind, ElementKind::Text { .. }) {
            let mut resized = selection::resize_text(
                &original.kind,
                original.style,
                original.bounds,
                self.handle,
                delta,
                modifiers,
                self.text_size_range,
            )
            .map_or_else(
                || original.clone(),
                |(kind, style, bounds)| ResizeSnapshot {
                    kind,
                    style,
                    bounds,
                },
            );
            let [min, max] = self.text_size_range;
            let size = resized.style.size + (self.properties.size - original.style.size);
            resized.set_style(Style {
                size: size.clamp(min, max),
                ..self.properties
            });
            resized
        } else {
            let kind = selection::resize(
                &original.kind,
                self.handle,
                delta,
                original.style.roundness,
                modifiers,
                &mut self.equal_side_anchor,
            );
            ResizeSnapshot {
                bounds: bounds_for(&kind, self.properties),
                kind,
                style: self.properties,
            }
        };
        let changed = resized != self.current;
        self.current = resized;
        changed
    }

    /// Applies a style adjustment made during the resize.
    pub(super) fn set_style(&mut self, style: Style) -> bool {
        if style == self.current.style {
            return false;
        }
        // Text resizing also changes font size; retain only the user's adjustment.
        self.properties = Style {
            size: self.properties.size + (style.size - self.current.style.size),
            ..style
        };
        self.current.set_style(style);
        true
    }
}

#[derive(Debug)]
pub(super) enum Interaction {
    Freehand(crate::tool::ToolId, freehand::LiveStroke),
    Drawing {
        preset: crate::tool::ToolId,
        tool: Primitive,
        style: Style,
        start: Point,
        current: Point,
        modifiers: Modifiers,
    },
    Moving {
        ids: Vec<ElementId>,
        start: Point,
        current: Point,
    },
    Resizing(Resize),
    EditingText(Arc<str>, TextEdit),
    Erasing {
        tool: crate::tool::ToolId,
        previous: Point,
        size: f32,
    },
}

impl Editor {
    pub(in crate::draw) fn modifiers_changed(&mut self, modifiers: Modifiers) -> bool {
        match &mut self.interaction {
            Some(Interaction::Drawing {
                modifiers: current, ..
            }) if *current != modifiers => {
                *current = modifiers;
                true
            }
            Some(Interaction::Resizing(resize)) => resize.update(resize.point, modifiers),
            _ => false,
        }
    }

    pub(in crate::draw) fn pointer_down(
        &mut self,
        point: Point,
        modifiers: Modifiers,
        tool_override: ToolOverride,
    ) -> bool {
        if tool_override == ToolOverride::None
            && let Some(edit) = self.text_edit_mut()
            && edit.bounds().contains(point)
        {
            return edit.click(point, 1, modifiers.shift);
        }
        let previous = self.finish_interaction();
        let effective_id = self.effective_tool(tool_override);
        let effective_tool = self.tools[effective_id.0].primitive;
        let effective_style = self.tool_style(effective_id);
        match effective_tool {
            Primitive::Eraser => {
                let size = effective_style.size;
                self.interaction = Some(Interaction::Erasing {
                    tool: effective_id,
                    previous: point,
                    size,
                });
                previous | self.erase_between(point, point, size)
            }
            Primitive::Pen => {
                self.interaction = Some(Interaction::Freehand(
                    effective_id,
                    freehand::LiveStroke::new(point, effective_style, self.presentation),
                ));
                true
            }
            Primitive::Line
            | Primitive::Arrow
            | Primitive::Triangle
            | Primitive::Rectangle
            | Primitive::Ellipse => {
                self.interaction = Some(Interaction::Drawing {
                    preset: effective_id,
                    tool: effective_tool,
                    style: effective_style,
                    start: point,
                    current: point,
                    modifiers,
                });
                true
            }
            Primitive::Text => {
                let origin = Point::new(
                    point.x,
                    point.y - text_line_height(effective_style.size) * 0.5,
                );
                let edit = self.make_text_edit(
                    None,
                    origin,
                    String::new(),
                    effective_style,
                    [1.0; 2],
                    self.tool_font(effective_id),
                );
                self.interaction = Some(Interaction::EditingText(
                    self.tools[effective_id.0].id.clone(),
                    edit,
                ));
                true
            }
            Primitive::Select => {
                if !modifiers.ctrl
                    && let [id] = self.selected[..]
                    && let Some(element) = self.element(id)
                    && let Some(handle) = selection::hit_handle(element, point)
                {
                    let tool =
                        ToolDefinition::preset(&self.tools, &element.preset, Primitive::Text);
                    let range = self.tools[tool.0].size_range;
                    let original = ResizeSnapshot {
                        kind: element.kind.clone(),
                        style: element.style,
                        bounds: element.bounds,
                    };
                    self.interaction = Some(Interaction::Resizing(Resize {
                        id,
                        preset: element.preset.clone(),
                        handle,
                        current: original.clone(),
                        text_size_range: [range.min, range.max],
                        start: point,
                        point,
                        properties: original.style,
                        original,
                        equal_side_anchor: None,
                    }));
                    return true;
                }
                let hit = self.hit_test(point);
                if modifiers.ctrl {
                    if let Some(id) = hit {
                        if let Some(index) =
                            self.selected.iter().position(|selected| *selected == id)
                        {
                            self.selected.remove(index);
                        } else {
                            self.selected.push(id);
                        }
                        return true;
                    }
                    return previous;
                }
                let Some(id) = hit else {
                    let changed = !self.selected.is_empty();
                    self.selected.clear();
                    return previous | changed;
                };
                if !self.selected.contains(&id) {
                    self.selected.clear();
                    self.selected.push(id);
                }
                let mut ids = self.selected.clone();
                ids.sort_unstable();
                self.interaction = Some(Interaction::Moving {
                    ids,
                    start: point,
                    current: point,
                });
                true
            }
        }
    }

    pub(in crate::draw) fn pointer_motion(&mut self, point: Point, modifiers: Modifiers) -> bool {
        match &mut self.interaction {
            Some(Interaction::EditingText(_, edit)) => edit.drag(point),
            Some(Interaction::Drawing {
                current,
                modifiers: current_modifiers,
                ..
            }) => {
                *current = point;
                *current_modifiers = modifiers;
                true
            }
            Some(Interaction::Moving { current, .. }) => {
                *current = point;
                true
            }
            Some(Interaction::Resizing(resize)) => resize.update(point, modifiers),
            Some(Interaction::Erasing { previous, size, .. }) => {
                let start = std::mem::replace(previous, point);
                let size = *size;
                self.erase_between(start, point, size)
            }
            Some(Interaction::Freehand(..)) | None => false,
        }
    }

    pub(in crate::draw) fn pen_motion(&mut self, motion: PenMotion, modifiers: Modifiers) -> bool {
        let Some(Interaction::Freehand(_, stroke)) = &mut self.interaction else {
            return false;
        };
        stroke.push_motion(motion, modifiers.shift)
    }

    pub(in crate::draw) fn pen_release_tip(
        &self,
        point: Point,
        modifiers: Modifiers,
    ) -> Option<Point> {
        let Some(Interaction::Freehand(_, stroke)) = &self.interaction else {
            return None;
        };
        Some(stroke.release_tip(point, modifiers.shift))
    }

    pub(in crate::draw) fn pointer_up(&mut self, point: Point, modifiers: Modifiers) -> bool {
        if let Some(edit) = self.text_edit_mut() {
            return edit.end_drag(Some(point));
        }
        match self.interaction.take() {
            Some(Interaction::Freehand(preset, stroke)) => {
                let (kind, style, geometry) = stroke.finish(point, modifiers.shift);
                let preset = self.tools[preset.0].id.clone();
                self.insert(preset, kind, style, Some(geometry));
                true
            }
            Some(Interaction::Drawing {
                preset,
                tool,
                style,
                start,
                ..
            }) => {
                // Without a drag, only rectangles and filled ellipses leave a visible dot.
                if point != start
                    || tool == Primitive::Rectangle
                    || (tool == Primitive::Ellipse && style.filled)
                {
                    let kind = drawing_kind(tool, start, point, modifiers);
                    self.insert(self.tools[preset.0].id.clone(), kind, style, None);
                }
                true
            }
            Some(Interaction::Moving { ids, start, .. }) => {
                if point != start {
                    self.document.update(false, ids, |element| {
                        Some((element.kind.translated(point - start), element.style))
                    });
                }
                true
            }
            Some(Interaction::Resizing(mut resize)) => {
                resize.update(point, modifiers);
                if resize.current != resize.original {
                    let mut next = Some((resize.current.kind, resize.current.style));
                    self.document.update(false, [resize.id], |_| next.take());
                }
                true
            }
            Some(Interaction::Erasing { previous, size, .. }) => {
                self.erase_between(previous, point, size)
            }
            interaction => {
                self.interaction = interaction;
                false
            }
        }
    }

    pub(in crate::draw) fn cursor(&self, point: Point, tool_override: ToolOverride) -> Cursor {
        let effective_id = self.effective_tool(tool_override);
        let cursor = self.default_cursor(point, tool_override);
        let cursor_tool = match &self.interaction {
            Some(
                Interaction::Freehand(tool, _)
                | Interaction::Drawing { preset: tool, .. }
                | Interaction::Erasing { tool, .. },
            ) => *tool,
            _ => effective_id,
        };
        let visible = self.tools[cursor_tool.0].cursor;
        if !visible
            && matches!(
                self.interaction,
                Some(Interaction::Freehand(..) | Interaction::Drawing { .. })
            )
        {
            return Cursor::Hidden;
        }
        match cursor {
            Cursor::Hidden | Cursor::Tool(_) if visible => Cursor::Shape(CursorHint::Crosshair),
            _ => cursor,
        }
    }

    fn default_cursor(&self, point: Point, tool_override: ToolOverride) -> Cursor {
        if let Some(Interaction::Erasing { tool, .. }) = &self.interaction {
            return self.tool_cursor(*tool);
        }
        let effective_id = self.effective_tool(tool_override);
        let effective_tool = self.tools[effective_id.0].primitive;
        if tool_override != ToolOverride::None && effective_tool == Primitive::Eraser {
            return self.tool_cursor(effective_id);
        }
        Cursor::Shape(match &self.interaction {
            Some(Interaction::Resizing(resize)) => selection::cursor(resize.handle),
            Some(Interaction::Freehand(..)) => return Cursor::Hidden,
            Some(Interaction::Drawing { .. }) => CursorHint::Crosshair,
            Some(Interaction::Moving { .. }) => CursorHint::Move,
            Some(Interaction::EditingText(..)) => CursorHint::Text,
            _ if self.picker.is_some() => CursorHint::Crosshair,
            _ => match effective_tool {
                Primitive::Text => CursorHint::Text,
                Primitive::Line
                | Primitive::Arrow
                | Primitive::Triangle
                | Primitive::Rectangle
                | Primitive::Ellipse => CursorHint::Crosshair,
                Primitive::Pen | Primitive::Eraser => return self.tool_cursor(effective_id),
                Primitive::Select => {
                    let element = match self.selected[..] {
                        [id] => self.element(id),
                        _ => None,
                    };
                    match element.map(|element| (element, selection::hit_handle(element, point))) {
                        Some((_, Some(handle))) => selection::cursor(handle),
                        Some((element, None))
                            if element.hit_test(point, 1.0, self.presentation_settings) =>
                        {
                            CursorHint::Move
                        }
                        _ => CursorHint::Crosshair,
                    }
                }
            },
        })
    }

    fn tool_cursor(&self, id: crate::tool::ToolId) -> Cursor {
        let tool = self.tools[id.0].primitive;
        let mut style = self.tool_style(id);
        if tool == Primitive::Eraser
            && let Some(Interaction::Erasing { size, .. }) = &self.interaction
        {
            style.size = *size;
        }
        Cursor::Tool(ToolCursor {
            tool,
            size: style.size,
            roundness: style.roundness,
            color: style.color,
        })
    }

    pub(super) fn cancel_interaction(&mut self) -> bool {
        // Every interaction or selection change ends a merged undo step.
        self.document.history.seal();
        self.interaction.take().is_some()
    }

    pub(in crate::draw) fn finish_pointer_interaction(
        &mut self,
        point: Option<Point>,
        modifiers: Modifiers,
    ) -> bool {
        if let Some(edit) = self.text_edit_mut() {
            return edit.end_drag(point);
        }
        if self.interaction.is_none() {
            return false;
        }
        match point {
            Some(point) => self.pointer_up(point, modifiers),
            None => self.cancel_interaction(),
        }
    }

    pub(super) fn finish_interaction(&mut self) -> bool {
        if self.is_editing_text() {
            self.commit_text()
        } else {
            self.cancel_interaction()
        }
    }
}
