use super::super::scene::{Element, ElementId, ElementKind, HIT_SLOP, Point, Style};
use super::super::text_edit::TextEdit;
use super::{Damage, Editor, HistoryEntry, Interaction};
use crate::render::Geometry;

impl Editor {
    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    pub fn update_text_bounds(
        &mut self,
        mut layout_size: impl FnMut(ElementId) -> Option<[f32; 2]>,
    ) {
        for element in &mut self.elements {
            if !matches!(element.kind, ElementKind::Text { .. }) {
                continue;
            }
            if let Some(size) = layout_size(element.id) {
                element.update_text_bounds(size);
            }
        }
    }

    pub fn double_click_at(&mut self, point: Point) -> Damage {
        if self.tool != super::super::tool::Tool::Select {
            return Damage::None;
        }
        let [id] = self.selected.as_slice() else {
            return Damage::None;
        };
        let id = *id;
        let Some(element) = self.element(id) else {
            return Damage::None;
        };
        if matches!(element.kind, ElementKind::Text { .. }) && element.hit_test(point) {
            return self.begin_text_edit(id);
        }
        Damage::None
    }

    pub fn hit_test(&self, point: Point) -> Option<ElementId> {
        self.elements
            .iter()
            .rev()
            .find(|element| {
                element.bounds.expanded(HIT_SLOP).contains(point) && element.hit_test(point)
            })
            .map(|element| element.id)
    }

    pub fn undo(&mut self) -> Damage {
        let cancelled = self.cancel_interaction();
        if !self.history.undo(&mut self.elements) {
            return cancelled;
        }
        self.selected.clear();
        Damage::Scene
    }

    pub fn redo(&mut self) -> Damage {
        let cancelled = self.cancel_interaction();
        if !self.history.redo(&mut self.elements) {
            return cancelled;
        }
        self.selected.clear();
        Damage::Scene
    }

    pub(super) fn select_all(&mut self) -> Damage {
        let cancelled = self.cancel_interaction();
        if self.elements.is_empty() {
            return cancelled;
        }
        let damage = cancelled.max(self.switch_tool(super::super::tool::Tool::Select));
        let selected = self.elements.iter().map(|element| element.id).collect();
        if self.selected == selected {
            return damage;
        }
        self.selected = selected;
        damage.max(Damage::Preview)
    }

    pub(super) fn clear(&mut self) -> Damage {
        let cancelled = self.cancel_interaction();
        if self.elements.is_empty() {
            return cancelled;
        }
        let elements = std::mem::take(&mut self.elements);
        self.history.record(HistoryEntry::Clear(elements));
        self.selected.clear();
        Damage::Scene
    }

    pub(super) fn delete_selection(&mut self) -> Damage {
        let selected = std::mem::take(&mut self.selected);
        if selected.is_empty() {
            return Damage::None;
        }
        let cancelled = self.cancel_interaction();
        if selected.len() == self.elements.len() {
            let elements = std::mem::take(&mut self.elements);
            self.history.record(HistoryEntry::Clear(elements));
            return cancelled.max(Damage::Scene);
        }
        cancelled.max(Damage::from_scene(self.remove_ids(&selected)))
    }

    fn remove_ids(&mut self, ids: &[ElementId]) -> bool {
        let mut removed = Vec::with_capacity(ids.len());
        for index in (0..self.elements.len()).rev() {
            if ids.contains(&self.elements[index].id) {
                removed.push((index, self.elements.remove(index)));
            }
        }
        if removed.is_empty() {
            return false;
        }
        removed.reverse();
        self.history.record(HistoryEntry::Remove(removed));
        self.selected.retain(|selected| !ids.contains(selected));
        true
    }

    pub(super) fn insert_kind(&mut self, kind: ElementKind, style: Style) {
        self.insert_kind_with_geometry(kind, style, None);
    }

    pub(super) fn insert_kind_with_geometry(
        &mut self,
        kind: ElementKind,
        style: Style,
        geometry: Option<Geometry>,
    ) {
        let element = match geometry {
            Some(geometry) => Element::with_geometry(self.next_id, kind, style, geometry),
            None => Element::new(self.next_id, kind, style),
        };
        self.next_id += 1;
        let index = self.elements.len();
        let id = element.id;
        self.elements.push(element);
        self.history.record(HistoryEntry::Insert(vec![(index, id)]));
    }

    fn remove_id(&mut self, id: ElementId) -> bool {
        self.remove_ids(&[id])
    }

    pub(super) fn erase_at(&mut self, point: Point) -> bool {
        let radius = self.eraser_width() * 0.5;
        let hit = self
            .elements
            .iter()
            .rev()
            .find(|element| element.erase_hit_test(point, radius))
            .map(|element| element.id);
        hit.is_some_and(|id| self.remove_id(id))
    }

    pub(super) fn commit_text(&mut self) -> Damage {
        let Some(Interaction::EditingText(TextEdit {
            id,
            origin,
            content,
            font_size,
            style,
            ..
        })) = self.interaction.take()
        else {
            return Damage::None;
        };
        if content.is_empty() {
            return id.map_or(Damage::Preview, |id| Damage::from_scene(self.remove_id(id)));
        }
        let kind = ElementKind::Text {
            origin,
            content,
            font_size,
        };
        if let Some(id) = id {
            let element = self.element_mut(id).expect("editing text exists");
            if element.kind == kind && element.style == style {
                return Damage::Preview;
            }
            let (kind, style) = element.replace(kind, style);
            self.history
                .record(HistoryEntry::Update(vec![(id, kind, style)]));
        } else {
            self.insert_kind(kind, style);
        }
        Damage::Scene
    }

    fn begin_text_edit(&mut self, id: ElementId) -> Damage {
        let Some(element) = self.element(id) else {
            return Damage::None;
        };
        let ElementKind::Text {
            origin,
            content,
            font_size,
        } = &element.kind
        else {
            return Damage::None;
        };
        self.interaction = Some(Interaction::EditingText(TextEdit {
            id: Some(id),
            origin: *origin,
            content: content.clone(),
            cursor: content.len(),
            font_size: *font_size,
            style: element.style,
        }));
        Damage::Scene
    }

    pub(super) fn element(&self, id: ElementId) -> Option<&Element> {
        self.elements.iter().find(|element| element.id == id)
    }

    pub(super) fn element_mut(&mut self, id: ElementId) -> Option<&mut Element> {
        self.elements.iter_mut().find(|element| element.id == id)
    }
}
