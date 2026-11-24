use super::{Editor, HistoryEntry, drawing_kind};
use crate::draw::scene::{Bounds, Element, ElementId, ElementKind, Point, Style, bounds_for};
use crate::draw::selection::{self, Handle};
use crate::draw::text_edit::TextEdit;
use crate::draw::{Cursor, Modifiers, PenMotion, ToolCursor, ToolOverride, freehand};
use crate::text::text_line_height;
use crate::tool::Primitive;

#[derive(Debug, Clone, PartialEq)]
pub(in crate::draw) struct ResizeSnapshot {
    pub(in crate::draw) kind: ElementKind,
    pub(in crate::draw) style: Style,
    pub(in crate::draw) bounds: Bounds,
}

impl From<&Element> for ResizeSnapshot {
    fn from(element: &Element) -> Self {
        Self {
            kind: element.kind.clone(),
            style: element.style,
            bounds: element.bounds,
        }
    }
}

impl ResizeSnapshot {
    pub(super) fn set_style(&mut self, style: Style) {
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
    Resizing {
        id: ElementId,
        preset: std::sync::Arc<str>,
        text_size_range: [f32; 2],
        handle: Handle,
        start: Point,
        point: Point,
        original: ResizeSnapshot,
        properties: Style,
        current: ResizeSnapshot,
        equal_side_anchor: Option<usize>,
    },
    EditingText(std::sync::Arc<str>, TextEdit),
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
            Some(Interaction::Resizing {
                text_size_range,
                handle,
                start,
                point,
                original,
                properties,
                current,
                equal_side_anchor,
                ..
            }) => {
                let resized = resize_element(
                    original,
                    *handle,
                    *point - *start,
                    modifiers,
                    equal_side_anchor,
                    *text_size_range,
                    *properties,
                );
                if resized == *current {
                    false
                } else {
                    *current = resized;
                    true
                }
            }
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
        if effective_tool == Primitive::Eraser {
            let size = effective_style.size;
            self.interaction = Some(Interaction::Erasing {
                tool: effective_id,
                previous: point,
                size,
            });
            return previous | self.erase_between(point, point, size);
        }

        match effective_tool {
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
                    && self.selected.len() == 1
                    && let Some(id) = self.selected.first().copied()
                    && let Some(handle) = self.hit_handle(id, point)
                    && let Some(element) = self.element(id)
                {
                    let text_size_range = self.text_size_range(element);
                    let preset = element.preset.clone();
                    let original = ResizeSnapshot::from(element);
                    self.interaction = Some(Interaction::Resizing {
                        id,
                        preset,
                        text_size_range,
                        handle,
                        start: point,
                        point,
                        current: original.clone(),
                        properties: original.style,
                        original,
                        equal_side_anchor: None,
                    });
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
            Primitive::Eraser => unreachable!(),
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
            Some(Interaction::Resizing {
                text_size_range,
                handle,
                start,
                point: current_point,
                original,
                properties,
                current,
                equal_side_anchor,
                ..
            }) => {
                *current = resize_element(
                    original,
                    *handle,
                    point - *start,
                    modifiers,
                    equal_side_anchor,
                    *text_size_range,
                    *properties,
                );
                *current_point = point;
                true
            }
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
                self.insert_element(Element::with_geometry(
                    self.next_id,
                    self.tools[preset.0].id.clone(),
                    kind,
                    style,
                    geometry,
                ));
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
                    self.insert_kind(
                        self.tools[preset.0].id.clone(),
                        drawing_kind(tool, start, point, modifiers),
                        style,
                    );
                }
                true
            }
            Some(Interaction::Moving { ids, start, .. }) => {
                if point != start {
                    let delta = point - start;
                    let mut elements = Vec::with_capacity(ids.len());
                    for id in ids {
                        let element = self.element_mut(id).expect("moving element exists");
                        let after = element.kind.translated(delta);
                        let style = element.style;
                        let (kind, style) = element.replace(after, style);
                        elements.push((id, kind, style));
                    }
                    self.document.history.record(HistoryEntry::Update(elements));
                }
                true
            }
            Some(Interaction::Resizing {
                id,
                text_size_range,
                handle,
                start,
                original,
                properties,
                mut equal_side_anchor,
                ..
            }) => {
                let current = resize_element(
                    &original,
                    handle,
                    point - start,
                    modifiers,
                    &mut equal_side_anchor,
                    text_size_range,
                    properties,
                );
                if current != original
                    && let Some(element) = self.element_mut(id)
                {
                    element.replace(current.kind, current.style);
                    self.document.history.record(HistoryEntry::Update(vec![(
                        id,
                        original.kind,
                        original.style,
                    )]));
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

    fn hit_handle(&self, id: ElementId, point: Point) -> Option<Handle> {
        let element = self.element(id)?;
        selection::hit_handle(&element.kind, element.style, element.bounds, point)
    }

    fn text_size_range(&self, element: &Element) -> [f32; 2] {
        let id = super::ToolDefinition::preset(&self.tools, &element.preset, Primitive::Text);
        let range = self.tools[id.0].size_range;
        [range.min, range.max]
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
            Cursor::Hidden | Cursor::Tool(_) if visible => {
                Cursor::Shape(selection::CursorHint::Crosshair)
            }
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
        match &self.interaction {
            Some(Interaction::Resizing { handle, .. }) => {
                return Cursor::Shape(selection::cursor(*handle));
            }
            Some(Interaction::Freehand(..)) => return Cursor::Hidden,
            Some(Interaction::Drawing { .. }) => {
                return Cursor::Shape(selection::CursorHint::Crosshair);
            }
            Some(Interaction::Moving { .. }) => {
                return Cursor::Shape(selection::CursorHint::Move);
            }
            Some(Interaction::EditingText(..)) => {
                return Cursor::Shape(selection::CursorHint::Text);
            }
            _ => {}
        }
        if self.picker.is_some() {
            return Cursor::Shape(selection::CursorHint::Crosshair);
        }
        if effective_tool == Primitive::Text {
            return Cursor::Shape(selection::CursorHint::Text);
        }
        if matches!(
            effective_tool,
            Primitive::Line
                | Primitive::Arrow
                | Primitive::Triangle
                | Primitive::Rectangle
                | Primitive::Ellipse
        ) {
            return Cursor::Shape(selection::CursorHint::Crosshair);
        }
        if effective_tool != Primitive::Select {
            return self.tool_cursor(effective_id);
        }
        if self.selected.len() != 1 {
            return Cursor::Shape(selection::CursorHint::Crosshair);
        }
        let id = self.selected[0];
        Cursor::Shape(match self.hit_handle(id, point) {
            Some(handle) => selection::cursor(handle),
            None if self.element(id).is_some_and(|element| {
                element.hit_test(point, 1.0, self.presentation_settings)
            }) =>
            {
                selection::CursorHint::Move
            }
            None => selection::CursorHint::Crosshair,
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

fn resize_element(
    original: &ResizeSnapshot,
    handle: Handle,
    delta: Point,
    modifiers: Modifiers,
    equal_side_anchor: &mut Option<usize>,
    text_size_range: [f32; 2],
    properties: Style,
) -> ResizeSnapshot {
    if matches!(original.kind, ElementKind::Text { .. }) {
        let mut resized = selection::resize_text(
            &original.kind,
            original.style,
            original.bounds,
            handle,
            delta,
            modifiers,
            text_size_range,
        )
        .map_or_else(
            || original.clone(),
            |(kind, style, bounds)| ResizeSnapshot {
                kind,
                style,
                bounds,
            },
        );
        let size = (resized.style.size + (properties.size - original.style.size))
            .clamp(text_size_range[0], text_size_range[1]);
        resized.set_style(Style { size, ..properties });
        return resized;
    }
    let kind = selection::resize(
        &original.kind,
        handle,
        delta,
        original.style.roundness,
        modifiers,
        equal_side_anchor,
    );
    ResizeSnapshot {
        bounds: bounds_for(&kind, properties),
        kind,
        style: properties,
    }
}
