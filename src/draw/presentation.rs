//! Assembles visible annotations and transient decorations for an output.

use super::decorations::{text_cursor_rectangle, text_preedit_span, tool_cursor_geometry};
use super::editor::LiveElement;
use super::scene::{self, ElementKind};
use super::{DrawState, Feedback, Point, TextInputSnapshot};
use crate::OutputId;
use crate::render::{Previews, SceneItem, Viewport, WgpuState};
use crate::text::TextSpec;

impl DrawState {
    pub fn render(
        &mut self,
        output: OutputId,
        origin: Point,
        scale: [f64; 2],
        presentation: super::Presentation,
        wgpu: &mut WgpuState,
        before_present: impl FnOnce(Option<TextInputSnapshot<'_>>),
    ) -> Result<(), String> {
        let margin = self.shadow_margin();
        let Some(damage) = self.outputs.get_mut(&output).filter(|damage| damage.dirty) else {
            return Ok(());
        };
        let size = wgpu.size();
        let viewport = kurbo::Rect::new(
            f64::from(origin.x),
            f64::from(origin.y),
            f64::from(origin.x) + f64::from(size[0]) / scale[0],
            f64::from(origin.y) + f64::from(size[1]) / scale[1],
        );
        damage.viewport = Some(viewport);
        let visible = scene::Bounds {
            min: origin,
            max: Point::new(viewport.x1 as f32, viewport.y1 as f32),
        }
        .expanded(margin as f32);
        let active_canvas = presentation.canvas == Some(self.editor.canvas);
        let active_text = active_canvas.then(|| self.editor.text_edit()).flatten();
        let elements = presentation
            .canvas
            .map(|canvas| self.editor.canvas_elements(canvas))
            .unwrap_or_default();
        // Committed items are cacheable while none of them animates or follows a gesture.
        let mut cacheable = presentation.interactive && active_text.is_none();
        let items = {
            let mut items = Vec::with_capacity(elements.len());
            let now = std::time::Instant::now();
            for element in elements {
                let remaining = self.editor.annotation_remaining(element, now);
                if remaining <= 0.0 {
                    // An annotation can expire before `handle_timeouts` bumps `committed_revision`.
                    cacheable = false;
                    continue;
                }
                // The element's bounds do not follow uncommitted edits.
                if let Some(edit) = active_text.filter(|edit| edit.id == Some(element.id)) {
                    items.push(SceneItem::Text(edit.spec(), 1.0));
                    continue;
                }
                let live = if active_canvas {
                    self.editor.live_element(element)
                } else {
                    LiveElement::from(element)
                };
                // Moving or resizing an element off-screen still invalidates its cached position.
                cacheable &= !live.resized && !live.moved;
                let overhang = live.kind.ink_overhang(live.style);
                if !live.bounds.expanded(overhang).intersects(visible) {
                    continue;
                }
                cacheable &= remaining >= 1.0;
                let ElementKind::Text {
                    origin,
                    text,
                    scale,
                } = live.kind
                else {
                    let (geometry, opacity) = if live.resized {
                        let geometry = scene::geometry(live.kind, live.style);
                        (std::borrow::Cow::Owned(geometry), remaining)
                    } else {
                        element.presentation_geometry(remaining, self.editor.presentation_settings)
                    };
                    let offset = [live.offset.x, live.offset.y];
                    items.push(SceneItem::Geometry(geometry, offset, opacity));
                    continue;
                };
                let origin = *origin + live.offset;
                let spec = scene::text_spec(text.layout(), origin, live.style, *scale);
                items.push(SceneItem::Text(spec, remaining));
            }
            if let Some(edit) = active_text.filter(|edit| edit.id.is_none()) {
                items.push(SceneItem::Text(edit.spec(), 1.0));
            }
            let feedback = self.feedback.as_ref().filter(|feedback| {
                presentation.interactive && viewport.contains(scene::kurbo_point(feedback.anchor))
            });
            if let Some(Feedback { text, anchor, .. }) = feedback {
                let layout = text.layout();
                let cursor_radius = self
                    .tool_cursor
                    .map_or(0.0, |(_, cursor)| cursor.size * 0.5);
                let top =
                    (anchor.y - layout.height() - cursor_radius.max(8.0) - 8.0).max(origin.y + 1.0);
                let right = (viewport.x1 as f32 - layout.width() - 1.0).max(origin.x + 1.0);
                let left = (anchor.x + 16.0).min(right);
                // Four offset dark copies outline the white label.
                let outline = [[-1.0, 0.0], [1.0, 0.0], [0.0, -1.0], [0.0, 1.0]];
                let outline = outline.map(|offset| (offset, [0.0, 0.0, 0.0, 0.9]));
                for ([x, y], color) in outline.into_iter().chain([([0.0; 2], [1.0; 4])]) {
                    items.push(SceneItem::Feedback(TextSpec {
                        layout,
                        left: left + x,
                        top: top + y,
                        font_size: 18.0,
                        color,
                        background_roundness: None,
                        scale: [1.0; 2],
                    }));
                }
            }
            items
        };

        let shown = |geometry: &crate::render::Geometry, margin: f64| {
            geometry.bounds().is_some_and(|bounds| {
                bounds.inflate(margin, margin).intersect(viewport).area() > 0.0
            })
        };
        let mut previews = Vec::new();
        if active_canvas {
            self.editor.append_preview_geometry(&mut previews);
        }
        previews.retain(|geometry| shown(geometry, 10.0));
        let annotation_previews = previews.len();
        let mut cursor_rectangle = None;
        if let Some(edit) = active_text.filter(|_| presentation.interactive) {
            let [scale_x, scale_y] = edit.scale;
            let area = edit.ime_area();
            cursor_rectangle = Some(text_cursor_rectangle(edit.origin, area, edit.scale, origin));
            if self.caret_visible
                && let Some(caret) = edit.caret()
            {
                previews.push(caret);
            }
            edit.decoration_rectangles(|[x, y, width, height], style| {
                previews.push(text_preedit_span(
                    edit.origin.x + x * scale_x,
                    edit.origin.x + (x + width) * scale_x,
                    edit.origin.y + y * scale_y,
                    height * scale_y,
                    style,
                ));
            });
        }
        if presentation.interactive {
            self.editor
                .append_selection_geometry(self.feedback.is_none(), &mut previews);
        }

        let picker = presentation
            .interactive
            .then(|| self.editor.picker_geometry(viewport))
            .flatten();
        let (annotations, decorations) = previews.split_at(annotation_previews);
        let cursor = self
            .tool_cursor
            .filter(|_| presentation.interactive)
            .map(|(point, cursor)| tool_cursor_geometry(point, cursor))
            .filter(|geometry| shown(geometry, margin));
        if wgpu.render(
            &items,
            Previews {
                annotations,
                decorations,
                cursor,
            },
            picker.as_ref(),
            Viewport {
                background: presentation.background,
                shadows: self.shadows,
                shadow_overlap: self.shadow_overlap,
                origin: [origin.x, origin.y],
                scale,
                interactive: presentation.interactive,
                revision: cacheable.then_some(self.committed_revision),
            },
            || {
                before_present(
                    active_text
                        .filter(|_| presentation.interactive)
                        .map(|edit| edit.snapshot(cursor_rectangle)),
                );
            },
        )? {
            damage.dirty = false;
        }
        Ok(())
    }
}
