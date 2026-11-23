use super::interaction::ResizeSnapshot;
use super::{Editor, Interaction, drawing_kind};
use crate::draw::picker::{picker_bounds, picker_geometry};
use crate::draw::scene::{Bounds, ElementId, ElementKind, Point, Style, bounds_for, geometry};
use crate::draw::selection;
use crate::render::Geometry;
use crate::tool::Primitive;

impl Editor {
    pub(in crate::draw) fn has_preview(&self) -> bool {
        matches!(
            self.interaction,
            Some(Interaction::Freehand(..) | Interaction::Drawing { .. })
        )
    }

    pub(in crate::draw) fn append_preview_geometry(&self, output: &mut Vec<Geometry>) {
        match &self.interaction {
            Some(Interaction::Freehand(_, stroke)) => output.push(stroke.tail_geometry()),
            Some(Interaction::Drawing {
                tool,
                style,
                start,
                current,
                modifiers,
                ..
            }) => output.push(geometry(
                &drawing_kind(*tool, *start, *current, *modifiers),
                *style,
            )),
            _ => {}
        }
    }

    pub(in crate::draw) fn interaction_bounds(&self) -> Option<kurbo::Rect> {
        let outline = |bounds: Bounds, style: Style, kind: &ElementKind| {
            let overhang = match kind {
                ElementKind::Text { scale, .. } => style.size * scale[0].abs().max(scale[1].abs()),
                _ => 0.0,
            };
            bounds.expanded(overhang.max(selection::OUTLINE_EXTENT))
        };
        let interaction = match &self.interaction {
            Some(Interaction::Freehand(_, stroke)) => Some(stroke.bounds()),
            Some(Interaction::Drawing {
                tool,
                style,
                start,
                current,
                modifiers,
                ..
            }) => Some(bounds_for(
                &drawing_kind(*tool, *start, *current, *modifiers),
                *style,
            )),
            Some(Interaction::Moving {
                ids,
                start,
                current,
            }) => {
                let offset = *current - *start;
                ids.iter()
                    .filter_map(|&id| self.element(id))
                    .map(|e| {
                        let b = outline(e.bounds, e.style, &e.kind);
                        Bounds {
                            min: b.min + offset,
                            max: b.max + offset,
                        }
                    })
                    .reduce(|a, b| Bounds::from_points([a.min, a.max, b.min, b.max]))
            }
            Some(Interaction::Resizing { current, .. }) => {
                Some(outline(current.bounds, current.style, &current.kind))
            }
            Some(Interaction::EditingText(_, edit)) => {
                let overhang = edit.style.size * edit.scale[0].abs().max(edit.scale[1].abs());
                Some(
                    edit.bounds()
                        .expanded(overhang.max(selection::OUTLINE_EXTENT)),
                )
            }
            Some(Interaction::Erasing { .. }) | None => None,
        }
        .map(|b| {
            kurbo::Rect::new(
                f64::from(b.min.x),
                f64::from(b.min.y),
                f64::from(b.max.x),
                f64::from(b.max.y),
            )
        });
        let picker = self.picker.map(|p| picker_bounds(p.center));
        match (interaction, picker) {
            (Some(a), Some(b)) => Some(a.union(b)),
            (bounds, None) | (None, bounds) => bounds,
        }
    }

    pub(in crate::draw) fn append_selection_geometry(
        &self,
        show_handles: bool,
        output: &mut Vec<Geometry>,
    ) {
        if self.primitive() != Primitive::Select {
            return;
        }
        if self.selected.len() > 1 {
            let bounds = Bounds::from_points(
                self.selected
                    .iter()
                    .filter_map(|&id| {
                        let offset = self.moving_offset(id).unwrap_or_default();
                        Some((self.element(id)?.bounds, offset))
                    })
                    .flat_map(|(bounds, offset)| [bounds.min + offset, bounds.max + offset]),
            );
            output.push(selection::outline(bounds.min, bounds.max));
            return;
        }
        let Some(&id) = self.selected.first() else {
            return;
        };
        let Some(element) = self.element(id) else {
            return;
        };
        if let Some(edit) = self.text_edit().filter(|edit| edit.id == Some(id)) {
            let bounds = edit.bounds();
            output.push(selection::outline(bounds.min, bounds.max));
            return;
        }
        let preview = self.resize_preview(id);
        let (kind, bounds) = preview.map_or((&element.kind, element.bounds), |current| {
            (&current.kind, current.bounds)
        });
        let offset = self.moving_offset(id).unwrap_or_default();
        if !matches!(
            kind,
            ElementKind::Segment { .. } | ElementKind::Triangle { .. }
        ) {
            output.push(selection::outline(bounds.min + offset, bounds.max + offset));
        }
        if !show_handles || self.interaction.is_some() {
            return;
        }
        selection::append_handles(kind, element.style, output);
    }

    pub(in crate::draw) fn picker_geometry(
        &self,
        viewport: kurbo::Rect,
    ) -> Option<crate::render::LocalGeometry<'_>> {
        let picker = self.picker?;
        let active = self
            .tool_wheel
            .iter()
            .position(|id| *id == self.tool)
            .or_else(|| {
                self.tool_wheel
                    .iter()
                    .position(|id| *id == self.color_tool())
            });
        let tools: Vec<_> = self
            .tool_wheel
            .iter()
            .map(|id| {
                let tool = &self.tools[id.0];
                (
                    &tool.icon,
                    tool.primitive.supports_fill() && self.tool_properties[id.0].filled,
                )
            })
            .collect();
        picker_geometry(
            picker,
            active,
            self.current_color(),
            &tools,
            &self.palette,
            viewport,
            self.presentation,
        )
    }

    pub(in crate::draw) fn resize_preview(&self, id: ElementId) -> Option<&ResizeSnapshot> {
        match &self.interaction {
            Some(Interaction::Resizing {
                id: resized,
                current,
                ..
            }) if *resized == id => Some(current),
            _ => None,
        }
    }

    pub(in crate::draw) fn moving_offset(&self, id: ElementId) -> Option<Point> {
        let Some(Interaction::Moving {
            ids,
            start,
            current,
        }) = &self.interaction
        else {
            return None;
        };
        ids.binary_search(&id).is_ok().then_some(*current - *start)
    }
}
