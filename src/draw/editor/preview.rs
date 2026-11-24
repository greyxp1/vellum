use super::{Editor, Interaction, drawing_kind};
use crate::draw::picker::{picker_bounds, picker_geometry};
use crate::draw::scene::{
    Bounds, Element, ElementKind, Point, Style, bounds_for, geometry, text_overhang,
};
use crate::draw::selection::{self, OUTLINE_EXTENT};
use crate::render::Geometry;
use crate::tool::Primitive;

/// An element as it currently appears while a gesture moves or resizes it.
pub(in crate::draw) struct LiveElement<'a> {
    pub(in crate::draw) kind: &'a ElementKind,
    pub(in crate::draw) style: Style,
    pub(in crate::draw) bounds: Bounds,
    pub(in crate::draw) offset: Point,
    /// Resized elements need fresh geometry instead of their cached one.
    pub(in crate::draw) resized: bool,
    pub(in crate::draw) moved: bool,
}

fn outline_bounds(bounds: Bounds, kind: &ElementKind, style: Style) -> Bounds {
    bounds.expanded(kind.ink_overhang(style).max(OUTLINE_EXTENT))
}

impl Editor {
    pub(in crate::draw) fn has_preview(&self) -> bool {
        matches!(
            self.interaction,
            Some(Interaction::Freehand(..) | Interaction::Drawing { .. })
        )
    }

    /// The kind and style of the shape being drawn.
    fn drawing(&self) -> Option<(ElementKind, Style)> {
        let Some(Interaction::Drawing {
            tool,
            style,
            start,
            current,
            modifiers,
            ..
        }) = &self.interaction
        else {
            return None;
        };
        Some((drawing_kind(*tool, *start, *current, *modifiers), *style))
    }

    pub(in crate::draw) fn append_preview_geometry(&self, output: &mut Vec<Geometry>) {
        if let Some(Interaction::Freehand(_, stroke)) = &self.interaction {
            output.push(stroke.tail_geometry());
        } else if let Some((kind, style)) = self.drawing() {
            output.push(geometry(&kind, style));
        }
    }

    pub(in crate::draw) fn interaction_bounds(&self) -> Option<kurbo::Rect> {
        let interaction = match &self.interaction {
            Some(Interaction::Freehand(_, stroke)) => Some(stroke.bounds()),
            Some(Interaction::Drawing { .. }) => {
                self.drawing().map(|(kind, style)| bounds_for(&kind, style))
            }
            Some(Interaction::Moving {
                ids,
                start,
                current,
            }) => ids
                .iter()
                .filter_map(|&id| self.element(id))
                .map(|e| outline_bounds(e.bounds.translated(*current - *start), &e.kind, e.style))
                .reduce(Bounds::union),
            Some(Interaction::Resizing(resize)) => {
                let current = &resize.current;
                Some(outline_bounds(current.bounds, &current.kind, current.style))
            }
            Some(Interaction::EditingText(_, edit)) => Some(
                edit.bounds()
                    .expanded(text_overhang(edit.style, edit.scale).max(OUTLINE_EXTENT)),
            ),
            Some(Interaction::Erasing { .. }) | None => None,
        }
        .map(kurbo::Rect::from);
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
        let mut selected = self
            .selected
            .iter()
            .filter_map(|&id| Some(self.live_element(self.element(id)?)));
        if self.selected.len() > 1 {
            let bounds = selected.map(|live| live.bounds).reduce(Bounds::union);
            output.push(selection::outline(bounds.unwrap_or_default()));
            return;
        }
        let Some(live) = selected.next() else {
            return;
        };
        if let Some(edit) = self
            .text_edit()
            .filter(|edit| edit.id == self.selected.first().copied())
        {
            output.push(selection::outline(edit.bounds()));
            return;
        }
        if !matches!(
            live.kind,
            ElementKind::Segment { .. } | ElementKind::Triangle { .. }
        ) {
            output.push(selection::outline(live.bounds));
        }
        if show_handles && self.interaction.is_none() {
            selection::append_handles(live.kind, live.style, output);
        }
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

    pub(in crate::draw) fn live_element<'a>(&'a self, element: &'a Element) -> LiveElement<'a> {
        match &self.interaction {
            Some(Interaction::Resizing(resize)) if resize.id == element.id => LiveElement {
                kind: &resize.current.kind,
                style: resize.current.style,
                bounds: resize.current.bounds,
                resized: true,
                ..element.into()
            },
            Some(Interaction::Moving {
                ids,
                start,
                current,
            }) if ids.binary_search(&element.id).is_ok() => LiveElement {
                bounds: element.bounds.translated(*current - *start),
                offset: *current - *start,
                moved: true,
                ..element.into()
            },
            _ => element.into(),
        }
    }
}

impl<'a> From<&'a Element> for LiveElement<'a> {
    fn from(element: &'a Element) -> Self {
        Self {
            kind: &element.kind,
            style: element.style,
            bounds: element.bounds,
            offset: Point::default(),
            resized: false,
            moved: false,
        }
    }
}
