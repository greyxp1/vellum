use super::super::scene::{Bounds, Element, ElementId, ElementKind, Point, Style, bounds_for};
use super::super::selection::{self, Handle};
use super::super::text_edit::TextEdit;
use super::super::tool::Tool;
use super::super::{Cursor, Modifiers, ToolCursor, ToolOverride, freehand};
use super::{Damage, Editor, HistoryEntry, drawing_kind};
use crate::render::text_line_height;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ResizeSnapshot {
    pub(super) kind: ElementKind,
    pub(super) style: Style,
    pub(super) bounds: Bounds,
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

#[derive(Debug)]
pub(super) enum Interaction {
    Freehand(freehand::LiveStroke),
    Drawing {
        tool: Tool,
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
        handle: Handle,
        start: Point,
        point: Point,
        original: ResizeSnapshot,
        current: ResizeSnapshot,
        equal_side_anchor: Option<usize>,
    },
    EditingText(TextEdit),
    Erasing,
}

impl Editor {
    pub fn modifiers_changed(&mut self, modifiers: Modifiers) -> Damage {
        let text_size_range = self.text_size_range();
        match &mut self.interaction {
            Some(Interaction::Drawing {
                modifiers: current, ..
            }) if *current != modifiers => {
                *current = modifiers;
                Damage::Preview
            }
            Some(Interaction::Resizing {
                handle,
                start,
                point,
                original,
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
                    text_size_range,
                );
                if resized == *current {
                    Damage::None
                } else {
                    *current = resized;
                    Damage::Preview
                }
            }
            _ => Damage::None,
        }
    }

    pub fn pointer_down(
        &mut self,
        point: Point,
        modifiers: Modifiers,
        tool_override: ToolOverride,
    ) -> Damage {
        let previous = self.finish_interaction();
        let effective_tool = tool_override.effective_tool(self.tool);
        if effective_tool == Tool::Eraser {
            self.interaction = Some(Interaction::Erasing);
            return previous.max(Damage::from_scene(self.erase_at(point)));
        }

        match effective_tool {
            Tool::Pen => {
                self.interaction = Some(Interaction::Freehand(freehand::LiveStroke::new(
                    point, self.style,
                )));
                previous.max(Damage::Preview)
            }
            Tool::Line | Tool::Arrow | Tool::Triangle | Tool::Rectangle | Tool::Ellipse => {
                self.interaction = Some(Interaction::Drawing {
                    tool: effective_tool,
                    start: point,
                    current: point,
                    modifiers,
                });
                previous.max(Damage::Preview)
            }
            Tool::Text => {
                let origin = Point::new(point.x, point.y - text_line_height(self.style.size) * 0.5);
                self.interaction = Some(Interaction::EditingText(TextEdit {
                    id: None,
                    origin,
                    content: String::new(),
                    cursor: 0,
                    style: self.style,
                    scale: [1.0; 2],
                }));
                previous.max(Damage::Preview)
            }
            Tool::Select => {
                if !modifiers.ctrl
                    && self.selected.len() == 1
                    && let Some(id) = self.selected.first().copied()
                    && let Some(handle) = self.hit_handle(id, point)
                    && let Some(element) = self.element(id)
                {
                    let original = ResizeSnapshot::from(element);
                    self.interaction = Some(Interaction::Resizing {
                        id,
                        handle,
                        start: point,
                        point,
                        current: original.clone(),
                        original,
                        equal_side_anchor: None,
                    });
                    return Damage::Scene;
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
                        return previous.max(Damage::Preview);
                    }
                    return previous;
                }
                let changed = hit.is_none_or(|id| !self.selected.contains(&id));
                if let Some(id) = hit {
                    if changed {
                        self.selected.clear();
                        self.selected.push(id);
                    }
                    self.interaction = Some(Interaction::Moving {
                        ids: self.selected.clone(),
                        start: point,
                        current: point,
                    });
                } else {
                    self.selected.clear();
                }
                if hit.is_some() {
                    Damage::Scene
                } else {
                    previous.max(Damage::from_preview(changed))
                }
            }
            Tool::Eraser => unreachable!(),
        }
    }

    pub fn pointer_motion(&mut self, point: Point, modifiers: Modifiers) -> Damage {
        let text_size_range = self.text_size_range();
        match self.interaction.take() {
            Some(Interaction::Freehand(mut stroke)) => {
                let changed = stroke.push(point, modifiers.shift);
                self.interaction = Some(Interaction::Freehand(stroke));
                Damage::from_preview(changed)
            }
            Some(Interaction::Drawing { tool, start, .. }) => {
                self.interaction = Some(Interaction::Drawing {
                    tool,
                    start,
                    current: point,
                    modifiers,
                });
                Damage::Preview
            }
            Some(Interaction::Moving { ids, start, .. }) => {
                self.interaction = Some(Interaction::Moving {
                    ids,
                    start,
                    current: point,
                });
                Damage::Preview
            }
            Some(Interaction::Resizing {
                id,
                handle,
                start,
                original,
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
                );
                self.interaction = Some(Interaction::Resizing {
                    id,
                    handle,
                    start,
                    point,
                    original,
                    current,
                    equal_side_anchor,
                });
                Damage::Preview
            }
            Some(Interaction::Erasing) => {
                self.interaction = Some(Interaction::Erasing);
                Damage::from_scene(self.erase_at(point))
            }
            interaction => {
                self.interaction = interaction;
                Damage::None
            }
        }
    }

    pub fn pen_motion(&mut self, points: &[Point], modifiers: Modifiers) -> Damage {
        let Some(Interaction::Freehand(stroke)) = &mut self.interaction else {
            return Damage::None;
        };
        Damage::from_preview(stroke.push_batch(points, modifiers.shift))
    }

    pub fn pointer_up(&mut self, point: Point, modifiers: Modifiers) -> Damage {
        let text_size_range = self.text_size_range();
        match self.interaction.take() {
            Some(Interaction::Freehand(stroke)) => {
                let (points, style, geometry) = stroke.finish(point, modifiers.shift);
                self.insert_kind_with_geometry(
                    ElementKind::Path {
                        points,
                        smooth: true,
                        end_marker: None,
                    },
                    style,
                    Some(geometry),
                );
                Damage::Scene
            }
            Some(Interaction::Drawing { tool, start, .. }) => {
                self.insert_kind(drawing_kind(tool, start, point, modifiers), self.style);
                Damage::Scene
            }
            Some(Interaction::Moving { ids, start, .. }) => {
                if point != start {
                    let delta = point - start;
                    let mut elements = Vec::with_capacity(ids.len());
                    for id in ids {
                        let element = self.element(id).expect("moving element exists");
                        let after = element.kind.translated(delta);
                        let style = element.style;
                        if let Some(element) = self.element_mut(id) {
                            let (kind, style) = element.replace(after, style);
                            elements.push((id, kind, style));
                        }
                    }
                    if !elements.is_empty() {
                        self.history.record(HistoryEntry::Update(elements));
                    }
                } else {
                    // Pointer-down removed the moving element from the committed GPU batch.
                }
                Damage::Scene
            }
            Some(Interaction::Resizing {
                id,
                handle,
                start,
                original,
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
                );
                if current != original
                    && let Some(element) = self.element_mut(id)
                {
                    element.replace(current.kind, current.style);
                    self.history.record(HistoryEntry::Update(vec![(
                        id,
                        original.kind,
                        original.style,
                    )]));
                }
                Damage::Scene
            }
            Some(Interaction::Erasing) => Damage::None,
            interaction => {
                self.interaction = interaction;
                Damage::None
            }
        }
    }

    fn hit_handle(&self, id: ElementId, point: Point) -> Option<Handle> {
        let element = self.element(id)?;
        selection::hit_handle(&element.kind, element.style, element.bounds, point)
    }

    fn text_size_range(&self) -> [f32; 2] {
        let range = self
            .size_ranges
            .get(&Tool::Text)
            .expect("text has a size range");
        [range.min(), range.max()]
    }

    pub fn cursor(&self, point: Point, tool_override: ToolOverride) -> Cursor {
        let effective_tool = tool_override.effective_tool(self.tool);
        if tool_override == ToolOverride::Eraser {
            return self.tool_cursor(effective_tool);
        }
        match &self.interaction {
            Some(Interaction::Resizing { handle, .. }) => {
                return Cursor::Shape(selection::cursor(*handle));
            }
            Some(Interaction::Freehand(_)) => return Cursor::Hidden,
            Some(Interaction::Drawing { .. }) => {
                return Cursor::Shape(selection::CursorHint::Crosshair);
            }
            Some(Interaction::Moving { .. }) => {
                return Cursor::Shape(selection::CursorHint::Move);
            }
            Some(Interaction::EditingText(_)) => {
                return Cursor::Shape(selection::CursorHint::Text);
            }
            Some(Interaction::Erasing) => return self.tool_cursor(Tool::Eraser),
            _ => {}
        }
        if self.picker.is_some() {
            return Cursor::Shape(selection::CursorHint::Crosshair);
        }
        if effective_tool == Tool::Text {
            return Cursor::Shape(selection::CursorHint::Text);
        }
        if matches!(
            effective_tool,
            Tool::Line | Tool::Arrow | Tool::Triangle | Tool::Rectangle | Tool::Ellipse
        ) {
            return Cursor::Shape(selection::CursorHint::Crosshair);
        }
        if effective_tool != Tool::Select {
            return self.tool_cursor(effective_tool);
        }
        if self.selected.len() != 1 {
            return Cursor::Shape(selection::CursorHint::Crosshair);
        }
        let id = self.selected[0];
        Cursor::Shape(match self.hit_handle(id, point) {
            Some(handle) => selection::cursor(handle),
            None if self
                .element(id)
                .is_some_and(|element| element.hit_test(point)) =>
            {
                selection::CursorHint::Move
            }
            None => selection::CursorHint::Crosshair,
        })
    }

    fn tool_cursor(&self, tool: Tool) -> Cursor {
        Cursor::Tool(ToolCursor {
            tool,
            size: self.size_for(tool),
            roundness: self.style.roundness,
            color: self.style.color,
        })
    }

    pub(super) fn eraser_size(&self) -> f32 {
        self.size_for(Tool::Eraser)
    }

    pub(super) fn cancel_interaction(&mut self) -> Damage {
        match self.interaction.take() {
            Some(Interaction::Moving { .. } | Interaction::Resizing { .. }) => Damage::Scene,
            Some(_) => Damage::Preview,
            None => Damage::None,
        }
    }

    pub(super) fn finish_interaction(&mut self) -> Damage {
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
) -> ResizeSnapshot {
    if matches!(original.kind, ElementKind::Text { .. }) {
        let (kind, style, bounds) = selection::resize_text(
            &original.kind,
            original.style,
            original.bounds,
            handle,
            delta,
            modifiers,
            text_size_range,
        );
        return ResizeSnapshot {
            kind,
            style,
            bounds,
        };
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
        bounds: bounds_for(&kind, original.style),
        kind,
        style: original.style,
    }
}
