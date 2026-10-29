//! Assembles visible annotations and transient decorations for an output.

use super::decorations::{text_cursor_rectangle, text_preedit_span, tool_cursor_geometry};
use super::scene::{self, ElementKind};
use super::{DrawState, Feedback, Point, TextInputSnapshot};
use crate::OutputId;
use crate::render::{SceneItem, TextSpec, Viewport, WgpuState};

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
        .expanded(1.0);
        let active_canvas = presentation.canvas == Some(self.editor.canvas);
        let active_text = active_canvas.then(|| self.editor.text_edit()).flatten();
        let elements = presentation
            .canvas
            .map(|canvas| self.editor.canvas_elements(canvas))
            .unwrap_or_default();
        let items = {
            let mut items = Vec::with_capacity(elements.len());
            for element in elements {
                // The element's bounds do not follow uncommitted edits.
                if let Some(edit) = active_text.filter(|edit| edit.id == Some(element.id)) {
                    items.push(SceneItem::Text(edit.spec()));
                    continue;
                }
                let preview = active_canvas
                    .then(|| self.editor.resize_preview(element.id))
                    .flatten();
                let offset = if active_canvas {
                    self.editor.moving_offset(element.id).unwrap_or_default()
                } else {
                    Point::default()
                };
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
                if !bounds.expanded(overhang).intersects(visible) {
                    continue;
                }
                let ElementKind::Text {
                    origin,
                    content,
                    scale,
                } = kind
                else {
                    items.push(SceneItem::Geometry(
                        preview
                            .map(|current| scene::geometry(&current.kind, current.style))
                            .map(std::borrow::Cow::Owned)
                            .unwrap_or(std::borrow::Cow::Borrowed(&element.geometry)),
                        [offset.x, offset.y],
                    ));
                    continue;
                };
                items.push(SceneItem::Text(TextSpec {
                    key: element.id,
                    content,
                    left: origin.x + offset.x,
                    top: origin.y + offset.y,
                    font_size: style.size,
                    color: style.color,
                    background_roundness: style.filled.then_some(style.roundness),
                    scale: *scale,
                }));
            }
            if let Some(edit) = active_text.filter(|edit| edit.id.is_none()) {
                items.push(SceneItem::Text(edit.spec()));
            }
            if let Some(Feedback {
                text: content,
                anchor: at,
                ..
            }) = self.feedback.as_ref().filter(|_| presentation.interactive)
            {
                const OUTLINE: [f32; 4] = [0.0, 0.0, 0.0, 0.9];
                for ([x, y], color) in [
                    ([15.0, 16.0], OUTLINE),
                    ([17.0, 16.0], OUTLINE),
                    ([16.0, 15.0], OUTLINE),
                    ([16.0, 17.0], OUTLINE),
                    ([16.0, 16.0], [1.0; 4]),
                ] {
                    items.push(SceneItem::Text(TextSpec {
                        key: u64::MAX - 30,
                        content,
                        left: at.x + x,
                        top: at.y + y,
                        font_size: 18.0,
                        color,
                        background_roundness: None,
                        scale: [1.0; 2],
                    }));
                }
            }
            items
        };

        self.previews.clear();
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
                self.previews.push(edit.caret());
            }
            edit.decoration_rectangles(|[x, y, width, height], style| {
                self.previews.push(text_preedit_span(
                    edit.origin.x + x * scale_x,
                    edit.origin.x + (x + width) * scale_x,
                    edit.origin.y + y * scale_y,
                    height * scale_y,
                    style,
                ));
            });
        }
        if active_canvas {
            self.editor.append_preview_geometry(&mut self.previews);
        }
        if presentation.interactive {
            self.editor
                .append_selection_geometry(self.feedback.is_none(), &mut self.previews);
            if let Some((point, cursor)) = self.tool_cursor {
                self.previews.push(tool_cursor_geometry(point, cursor));
            }
        }
        self.previews.retain(|geometry| {
            geometry
                .bounds()
                .is_some_and(|bounds| bounds.inflate(1.0, 1.0).intersect(viewport).area() > 0.0)
        });
        let picker = presentation
            .interactive
            .then(|| self.editor.picker_geometry(viewport))
            .flatten();
        if wgpu.render(
            &items,
            &self.previews,
            picker.as_ref(),
            Viewport {
                background: presentation.background,
                origin: [origin.x, origin.y],
                scale,
            },
            active_text.map(|edit| (edit.id.unwrap_or(0), edit.layout())),
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
