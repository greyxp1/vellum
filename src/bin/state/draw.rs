mod editor;
mod freehand;
mod history;
mod picker;
mod scene;
mod selection;
mod text_edit;
mod tool;
mod triangle;

use crate::render::{FillRule, Geometry, LocalGeometry, TextSpec, WgpuState};
use std::time::{Duration, Instant};

pub(crate) use self::editor::{Action, CursorMove};
use self::editor::{Damage, Editor, EditorEffect};
use self::scene::ElementKind;
pub(super) use self::scene::Point;
pub(crate) use self::selection::CursorHint;
pub(crate) use self::tool::Tool;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ToolOverride {
    #[default]
    None,
    Eraser,
}

impl ToolOverride {
    pub(crate) fn from_eraser(enabled: bool) -> Self {
        if enabled { Self::Eraser } else { Self::None }
    }

    fn effective_tool(self, active: Tool) -> Tool {
        match self {
            Self::None => active,
            Self::Eraser => Tool::Eraser,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ToolCursor {
    pub tool: Tool,
    pub width: f32,
    pub roundness: f32,
    pub color: [f32; 4],
}

pub(crate) const MIN_TOOL_SIZE: f32 = 2.0;
pub(crate) const MAX_STROKE_WIDTH: f32 = 100.0;
pub(crate) const MAX_FONT_SIZE: f32 = 200.0;
pub(crate) const STABILIZER_FOLLOW: f32 = 0.35;
const CIRCLE_KAPPA: f64 = 0.552_284_749_830_793_6;

pub(crate) fn stabilizer_delay(width: f32) -> f32 {
    (width * 0.15).clamp(4.0, 16.0)
}

pub(crate) fn eraser_radius(width: f32) -> f32 {
    width.max(MIN_TOOL_SIZE) * 0.5
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Cursor {
    Hidden,
    Shape(CursorHint),
    Tool(ToolCursor),
}

impl Cursor {
    pub(crate) fn same_compositor_cursor(self, other: Self) -> bool {
        self == other
            || matches!(self, Self::Hidden | Self::Tool(_))
                && matches!(other, Self::Hidden | Self::Tool(_))
    }
}

const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

pub struct DrawState {
    editor: Editor,
    damage: Damage,
    feedback: Option<(String, Point)>,
    property_feedback_anchor: Option<Point>,
    feedback_until: Option<Instant>,
    feedback_duration: Duration,
    caret_visible: bool,
    caret_until: Option<Instant>,
    tool_cursor: Option<(Point, ToolCursor)>,
    previews: Vec<Geometry>,
    picker: Option<LocalGeometry>,
}

impl DrawState {
    pub(super) fn new(settings: crate::Settings) -> Self {
        Self {
            editor: Editor::new(
                settings.stroke_width,
                settings.stroke_color,
                settings.default_tool,
                settings.remember_last_tool,
                settings.default_fill_shapes,
                &settings.tool_defaults,
                settings.palette,
            ),
            damage: Damage::Scene,
            feedback: None,
            property_feedback_anchor: None,
            feedback_until: None,
            feedback_duration: settings.feedback_duration,
            caret_visible: true,
            caret_until: None,
            tool_cursor: None,
            previews: Vec::new(),
            picker: None,
        }
    }

    pub fn activate(&mut self) -> bool {
        let damage = self.editor.activate();
        self.record(damage)
    }

    pub fn deactivate(&mut self) -> bool {
        let mut damage = self.editor.deactivate();
        if self.feedback.take().is_some()
            | self.property_feedback_anchor.take().is_some()
            | self.feedback_until.take().is_some()
            | self.tool_cursor.take().is_some()
        {
            damage = damage.max(Damage::Preview);
        }
        self.caret_until = None;
        self.record(damage)
    }

    pub fn is_editing_text(&self) -> bool {
        self.editor.is_editing_text()
    }

    pub fn is_drawing_pen(&self) -> bool {
        self.editor.is_drawing_pen()
    }

    pub fn handle_action(&mut self, action: Action, at: Option<Point>) -> EditorEffect {
        let mut effect = self.editor.handle_action(action);
        if let (Some(label), Some(at)) = (effect.feedback.take(), at) {
            self.property_feedback_anchor = Some(at);
            self.feedback = Some((label, at));
            self.feedback_until = Some(Instant::now() + self.feedback_duration);
        }
        if self.editor.is_editing_text() {
            if self.show_caret() {
                effect.damage = effect.damage.max(Damage::Preview);
            }
        } else {
            self.caret_until = None;
        }
        self.damage.merge(effect.damage);
        effect
    }

    pub fn pointer_down(
        &mut self,
        point: Point,
        modifiers: Modifiers,
        tool_override: ToolOverride,
    ) -> bool {
        let damage = self.editor.pointer_down(point, modifiers, tool_override);
        if self.editor.is_editing_text() {
            self.show_caret();
        } else {
            self.caret_until = None;
        }
        self.record(damage)
    }

    pub fn pointer_motion(&mut self, point: Point, modifiers: Modifiers) -> bool {
        let damage = self.editor.pointer_motion(point, modifiers);
        self.record(damage)
    }

    pub fn modifiers_changed(&mut self, modifiers: Modifiers) -> bool {
        let damage = self.editor.modifiers_changed(modifiers);
        self.record(damage)
    }

    pub fn pointer_up(&mut self, point: Point, modifiers: Modifiers) -> bool {
        let damage = self.editor.pointer_up(point, modifiers);
        self.record(damage)
    }

    pub fn picker_active(&self) -> bool {
        self.editor.picker_active()
    }

    pub fn cursor(&self, point: Point, tool_override: ToolOverride) -> Cursor {
        self.editor.cursor(point, tool_override)
    }

    pub fn set_tool_cursor(&mut self, cursor: Option<(Point, ToolCursor)>) -> bool {
        if self.tool_cursor == cursor {
            return false;
        }
        self.tool_cursor = cursor;
        self.damage.merge(Damage::Preview);
        true
    }

    pub fn open_picker(&mut self, center: Point) -> bool {
        let damage = self.editor.open_picker(center);
        self.record(damage)
    }

    pub fn picker_motion(&mut self, point: Point) -> bool {
        let damage = self.editor.picker_motion(point);
        self.record(damage)
    }

    pub fn picker_release(&mut self, point: Point, latch_center: bool) -> bool {
        let damage = self.editor.picker_release(point, latch_center);
        self.record(damage)
    }

    pub fn dismiss_picker(&mut self) -> bool {
        let damage = self.editor.dismiss_picker();
        self.record(damage)
    }

    pub fn double_click_at(&mut self, point: Point) -> bool {
        let damage = self.editor.double_click_at(point);
        if self.editor.is_editing_text() {
            self.show_caret();
        }
        self.record(damage)
    }

    pub fn adjust(&mut self, steps: f32, at: Point, modifiers: Modifiers) -> bool {
        let (damage, feedback) = if modifiers.shift && !self.editor.is_editing_text() {
            self.editor.adjust_roundness(steps)
        } else if modifiers.ctrl {
            self.editor.adjust_opacity(steps)
        } else {
            self.editor.adjust_size(steps)
        };
        if damage.changed() {
            let anchor = *self.property_feedback_anchor.get_or_insert(at);
            self.feedback = Some((feedback, anchor));
            self.feedback_until = Some(Instant::now() + self.feedback_duration);
            self.damage.merge(damage);
        }
        damage.changed()
    }

    pub fn needs_render(&self) -> bool {
        self.damage.changed()
    }

    pub fn damage_scene(&mut self) {
        self.damage.merge(Damage::Scene);
    }

    fn record(&mut self, damage: Damage) -> bool {
        self.damage.merge(damage);
        damage.changed()
    }

    pub fn next_wakeup(&self) -> Option<Instant> {
        [self.feedback_until, self.caret_until]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn handle_timeouts(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.feedback_until.is_some_and(|until| now >= until) {
            self.feedback = None;
            self.property_feedback_anchor = None;
            self.feedback_until = None;
            changed = true;
        }
        if self.caret_until.is_some_and(|until| now >= until) {
            if self.editor.is_editing_text() {
                self.caret_visible = !self.caret_visible;
                self.caret_until = Some(now + CARET_BLINK_INTERVAL);
                changed = true;
            } else {
                self.caret_until = None;
            }
        }
        if changed {
            self.damage.merge(Damage::Preview);
        }
        changed
    }

    pub fn render(&mut self, wgpu: &mut WgpuState) {
        if !self.damage.changed() {
            return;
        }
        if self.damage == Damage::Scene {
            wgpu.set_committed_geometry(
                self.editor
                    .elements()
                    .iter()
                    .filter(|element| !self.editor.element_is_previewed(element.id))
                    .map(|element| &element.geometry),
            );
        }

        let editing_id = self.editor.active_text().and_then(|edit| edit.id);
        let mut caret = None;
        {
            let active_text = self.editor.active_text();
            let mut text_specs = Vec::new();
            for element in self.editor.elements() {
                if Some(element.id) == editing_id {
                    continue;
                }
                let ElementKind::Text {
                    origin,
                    content,
                    font_size,
                } = &element.kind
                else {
                    continue;
                };
                let offset = self.editor.moving_offset(element.id).unwrap_or_default();
                text_specs.push(TextSpec {
                    key: element.id,
                    content,
                    left: origin.x + offset.x,
                    top: origin.y + offset.y,
                    size: *font_size,
                    color: element.style.color,
                });
            }
            if let Some(edit) = active_text {
                let key = edit.id.unwrap_or(0);
                text_specs.push(TextSpec {
                    key,
                    content: &edit.content,
                    left: edit.origin.x,
                    top: edit.origin.y,
                    size: edit.font_size,
                    color: edit.style.color,
                });
                caret = Some((key, edit.cursor, edit.origin, edit.font_size));
            }
            if let Some((content, at)) = &self.feedback {
                for (index, [x, y]) in [[15.0, 16.0], [17.0, 16.0], [16.0, 15.0], [16.0, 17.0]]
                    .into_iter()
                    .enumerate()
                {
                    text_specs.push(TextSpec {
                        key: u64::MAX - 34 + index as u64,
                        content,
                        left: at.x + x,
                        top: at.y + y,
                        size: 18.0,
                        color: [0.0, 0.0, 0.0, 0.9],
                    });
                }
                text_specs.push(TextSpec {
                    key: u64::MAX - 30,
                    content,
                    left: at.x + 16.0,
                    top: at.y + 16.0,
                    size: 18.0,
                    color: [1.0, 1.0, 1.0, 1.0],
                });
            }
            wgpu.prepare_text(&text_specs);
        }
        self.editor
            .update_text_bounds(|id| wgpu.text_layout_size(id));

        self.previews.clear();
        if self.caret_visible
            && let Some((key, cursor, origin, font_size)) = caret
            && let Some(x) = wgpu.text_cursor_x(key, cursor)
        {
            self.previews
                .push(text_caret(origin.x + x, origin.y, font_size));
        }
        self.editor.append_preview_geometry(&mut self.previews);
        self.editor
            .append_selection_geometry(self.property_feedback_anchor.is_none(), &mut self.previews);
        if let Some((point, cursor)) = self.tool_cursor {
            self.previews.push(tool_cursor_geometry(point, cursor));
        }
        self.picker = self.editor.picker_geometry();
        if wgpu.render(&self.previews, self.picker.as_ref()) {
            self.damage = Damage::None;
        }
    }

    pub fn force_render(&mut self, wgpu: &mut WgpuState) {
        self.damage = Damage::Scene;
        self.render(wgpu);
    }

    fn show_caret(&mut self) -> bool {
        let changed = !self.caret_visible;
        self.caret_visible = true;
        self.caret_until = Some(Instant::now() + CARET_BLINK_INTERVAL);
        changed
    }
}

fn tool_cursor_geometry(point: Point, cursor: ToolCursor) -> Geometry {
    use kurbo::Shape;

    let radius = f64::from(match cursor.tool {
        Tool::Pen => cursor.width * 0.5,
        Tool::Eraser => eraser_radius(cursor.width),
        _ => unreachable!("only pen and eraser have tool cursors"),
    })
    .max(1.0);
    let center = kurbo::Point::new(f64::from(point.x), f64::from(point.y));
    if cursor.tool == Tool::Eraser {
        const OUTLINE_WIDTH: f64 = 0.75;
        let mut geometry = Geometry::fill(
            kurbo::Circle::new(center, radius + OUTLINE_WIDTH).to_path(0.1),
            FillRule::NonZero,
            [0.0, 0.0, 0.0, 1.0],
        );
        geometry.append(Geometry::fill(
            kurbo::Circle::new(center, radius).to_path(0.1),
            FillRule::NonZero,
            [1.0, 1.0, 1.0, 1.0],
        ));
        return geometry;
    }

    let mut color = cursor.color;
    color[3] = color[3].sqrt();
    let corner_radius = radius * f64::from(cursor.roundness.clamp(0.0, 1.0));
    Geometry::fill(
        kurbo::RoundedRect::new(
            center.x - radius,
            center.y - radius,
            center.x + radius,
            center.y + radius,
            corner_radius,
        )
        .to_path(0.1),
        FillRule::NonZero,
        color,
    )
}

fn text_caret(left: f32, top: f32, font_size: f32) -> Geometry {
    use kurbo::Shape;

    let bottom = top + font_size * 1.25;
    let black = [0.0, 0.0, 0.0, 1.0];
    let white = [1.0, 1.0, 1.0, 1.0];
    let mut geometry = Geometry::fill(
        kurbo::Rect::new(
            f64::from(left - 1.0),
            f64::from(top - 1.0),
            f64::from(left + 2.0),
            f64::from(bottom + 1.0),
        )
        .to_path(0.1),
        FillRule::NonZero,
        black,
    );
    geometry.append(Geometry::fill(
        kurbo::Rect::new(
            f64::from(left),
            f64::from(top),
            f64::from(left + 1.0),
            f64::from(bottom),
        )
        .to_path(0.1),
        FillRule::NonZero,
        white,
    ));
    geometry
}
