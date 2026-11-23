//! Assembles visible annotations and transient decorations for an output.

use super::decorations::{text_cursor_rectangle, text_preedit_span, tool_cursor_geometry};
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
        .expanded(if self.shadows { 10.0 } else { 1.0 });
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
                let preview = active_canvas
                    .then(|| self.editor.resize_preview(element.id))
                    .flatten();
                let moving = active_canvas
                    .then(|| self.editor.moving_offset(element.id))
                    .flatten();
                let offset = moving.unwrap_or_default();
                let bounds = preview.map_or(element.bounds, |current| current.bounds);
                let (kind, style) = preview.map_or((&element.kind, element.style), |current| {
                    (&current.kind, current.style)
                });
                // Layout bounds exclude glyph ink overhangs; allow one em around text.
                let overhang = match kind {
                    ElementKind::Text { scale, .. } => {
                        style.size * scale[0].abs().max(scale[1].abs())
                    }
                    _ => 0.0,
                };
                let bounds = scene::Bounds {
                    min: bounds.min + offset,
                    max: bounds.max + offset,
                };
                // Moving or resizing an element off-screen still invalidates its cached position.
                cacheable &= preview.is_none() && moving.is_none();
                if !bounds.expanded(overhang).intersects(visible) {
                    continue;
                }
                cacheable &= remaining >= 1.0;
                let ElementKind::Text {
                    origin,
                    text,
                    scale,
                } = kind
                else {
                    let (geometry, opacity) = if let Some(preview) = preview {
                        (
                            std::borrow::Cow::Owned(scene::geometry(&preview.kind, preview.style)),
                            remaining,
                        )
                    } else {
                        element.presentation_geometry(remaining, self.editor.presentation_settings)
                    };
                    items.push(SceneItem::Geometry(geometry, [offset.x, offset.y], opacity));
                    continue;
                };
                items.push(SceneItem::Text(
                    TextSpec {
                        layout: text.layout(),
                        left: origin.x + offset.x,
                        top: origin.y + offset.y,
                        font_size: style.size,
                        color: style.color,
                        background_roundness: style.filled.then_some(style.roundness),
                        scale: *scale,
                    },
                    remaining,
                ));
            }
            if let Some(edit) = active_text.filter(|edit| edit.id.is_none()) {
                items.push(SceneItem::Text(edit.spec(), 1.0));
            }
            if let Some(Feedback {
                text, anchor: at, ..
            }) = self.feedback.as_ref().filter(|feedback| {
                presentation.interactive
                    && viewport
                        .contains((f64::from(feedback.anchor.x), f64::from(feedback.anchor.y)))
            }) {
                const OUTLINE: [f32; 4] = [0.0, 0.0, 0.0, 0.9];
                let layout = text.layout();
                let cursor_radius = self
                    .tool_cursor
                    .map_or(0.0, |(_, cursor)| cursor.size * 0.5);
                let top =
                    (at.y - layout.height() - cursor_radius.max(8.0) - 8.0).max(origin.y + 1.0);
                let right = (viewport.x1 as f32 - layout.width() - 1.0).max(origin.x + 1.0);
                let left = (at.x + 16.0).min(right);
                for ([x, y], color) in [
                    ([-1.0, 0.0], OUTLINE),
                    ([1.0, 0.0], OUTLINE),
                    ([0.0, -1.0], OUTLINE),
                    ([0.0, 1.0], OUTLINE),
                    ([0.0, 0.0], [1.0; 4]),
                ] {
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

        let mut previews = Vec::new();
        if active_canvas {
            self.editor.append_preview_geometry(&mut previews);
        }
        previews.retain(|geometry| {
            geometry
                .bounds()
                .is_some_and(|bounds| bounds.inflate(10.0, 10.0).intersect(viewport).area() > 0.0)
        });
        let annotation_previews = previews.len();
        let mut cursor_rectangle = None;
        if let Some(edit) = active_text.filter(|_| presentation.interactive) {
            let [scale_x, scale_y] = edit.scale;
            cursor_rectangle = Some(text_cursor_rectangle(
                edit.origin,
                edit.ime_area(),
                edit.scale,
                origin,
            ));
            if self.caret_visible && edit.shows_caret() {
                previews.push(edit.caret());
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
        let margin = if self.shadows { 10.0 } else { 1.0 };
        let cursor = self
            .tool_cursor
            .filter(|_| presentation.interactive)
            .map(|(point, cursor)| tool_cursor_geometry(point, cursor))
            .filter(|geometry| {
                geometry.bounds().is_some_and(|bounds| {
                    bounds.inflate(margin, margin).intersect(viewport).area() > 0.0
                })
            });
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
