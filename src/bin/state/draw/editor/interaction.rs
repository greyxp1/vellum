use super::super::scene::{ElementId, ElementKind, Point};
use super::super::selection::{self, Handle};
use super::super::text_edit::TextEdit;
use super::super::tool::Tool;
use super::super::{Cursor, Modifiers, ToolCursor, ToolOverride, freehand};
use super::{Damage, Editor, HistoryEntry, drawing_kind};

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
        roundness: f32,
        original: ElementKind,
        current: ElementKind,
        equal_side_anchor: Option<usize>,
    },
    EditingText(TextEdit),
    Erasing,
}

impl Editor {
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
                self.interaction = Some(Interaction::EditingText(TextEdit {
                    id: None,
                    origin: point,
                    content: String::new(),
                    cursor: 0,
                    font_size: self
                        .properties(Tool::Text)
                        .expect("text must have adjustable properties")
                        .size,
                    style: self.style,
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
                    let original = element.kind.clone();
                    self.interaction = Some(Interaction::Resizing {
                        id,
                        handle,
                        start: point,
                        roundness: element.style.roundness,
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
        match self.interaction.take() {
            Some(Interaction::Freehand(mut stroke)) => {
                let (changed, froze_chunk) = stroke.push(point);
                self.interaction = Some(Interaction::Freehand(stroke));
                if froze_chunk {
                    Damage::Scene
                } else {
                    Damage::from_preview(changed)
                }
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
                roundness,
                original,
                mut equal_side_anchor,
                ..
            }) => {
                let current = selection::resize(
                    &original,
                    handle,
                    point - start,
                    roundness,
                    modifiers,
                    &mut equal_side_anchor,
                );
                self.interaction = Some(Interaction::Resizing {
                    id,
                    handle,
                    start,
                    roundness,
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

    pub fn pointer_up(&mut self, point: Point, modifiers: Modifiers) -> Damage {
        match self.interaction.take() {
            Some(Interaction::Freehand(stroke)) => {
                let (points, style, geometry) = stroke.finish(point);
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
                roundness,
                original,
                mut equal_side_anchor,
                ..
            }) => {
                let current = selection::resize(
                    &original,
                    handle,
                    point - start,
                    roundness,
                    modifiers,
                    &mut equal_side_anchor,
                );
                if current != original
                    && let Some(element) = self.element_mut(id)
                {
                    let style = element.style;
                    element.replace(current, style);
                    self.history
                        .record(HistoryEntry::Update(vec![(id, original, style)]));
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

    pub fn cursor(&self, point: Point, tool_override: ToolOverride) -> Cursor {
        let effective_tool = tool_override.effective_tool(self.tool);
        if tool_override == ToolOverride::Eraser {
            return self.tool_cursor(effective_tool);
        }
        match &self.interaction {
            Some(Interaction::Drawing {
                tool: Tool::Triangle | Tool::Rectangle | Tool::Ellipse,
                ..
            }) => {
                return Cursor::Shape(selection::CursorHint::Crosshair);
            }
            Some(Interaction::Resizing {
                original:
                    ElementKind::Triangle { .. }
                    | ElementKind::Rectangle { .. }
                    | ElementKind::Ellipse { .. },
                ..
            }) => {
                return Cursor::Shape(selection::CursorHint::Crosshair);
            }
            Some(
                Interaction::Freehand(_)
                | Interaction::Drawing { .. }
                | Interaction::Moving { .. }
                | Interaction::Resizing { .. },
            ) => return Cursor::Hidden,
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
            width: self.width_for(tool),
            color: self.style.color,
        })
    }

    pub(super) fn eraser_width(&self) -> f32 {
        self.width_for(Tool::Eraser)
    }

    pub(super) fn cancel_interaction(&mut self) -> Damage {
        match self.interaction.take() {
            Some(Interaction::Moving { .. } | Interaction::Resizing { .. }) => Damage::Scene,
            Some(Interaction::Freehand(stroke)) if !stroke.cached().is_empty() => Damage::Scene,
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
