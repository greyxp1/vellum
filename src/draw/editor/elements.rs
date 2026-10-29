use super::{Editor, HistoryEntry, Interaction};
use crate::draw::scene::{Element, ElementId, ElementKind, Point, Style};
use crate::draw::text_edit::TextEdit;

impl Editor {
    /// Returns None for a miss, or Some(changed) when the text edit handles the click.
    pub(in crate::draw) fn text_click_at(&mut self, point: Point, clicks: u8) -> Option<bool> {
        if let Some(edit) = self.text_edit_mut() {
            return edit
                .bounds()
                .contains(point)
                .then(|| edit.click(point, clicks, false));
        }
        if self.tool != crate::tool::Tool::Select {
            return None;
        }
        let [id] = self.selected.as_slice() else {
            return None;
        };
        let id = *id;
        let element = self.element(id)?;
        if matches!(element.kind, ElementKind::Text { .. }) && element.hit_test(point) {
            let changed = self.begin_text_edit(id);
            if let Some(edit) = self.text_edit_mut() {
                edit.click(point, clicks, false);
            }
            return Some(changed);
        }
        None
    }

    pub(in crate::draw) fn hit_test(&self, point: Point) -> Option<ElementId> {
        self.document
            .elements
            .iter()
            .rev()
            .find(|element| element.hit_test(point))
            .map(|element| element.id)
    }

    pub(in crate::draw) fn undo(&mut self) -> bool {
        let cancelled = self.cancel_interaction();
        if !self.document.history.undo(&mut self.document.elements) {
            return cancelled;
        }
        self.selected.clear();
        true
    }

    pub(in crate::draw) fn redo(&mut self) -> bool {
        let cancelled = self.cancel_interaction();
        if !self.document.history.redo(&mut self.document.elements) {
            return cancelled;
        }
        self.selected.clear();
        true
    }

    pub(super) fn select_all(&mut self) -> bool {
        let cancelled = self.cancel_interaction();
        if self.document.elements.is_empty() {
            return cancelled;
        }
        let changed = cancelled | self.switch_tool(crate::tool::Tool::Select);
        let selected = self
            .document
            .elements
            .iter()
            .map(|element| element.id)
            .collect();
        if self.selected == selected {
            return changed;
        }
        self.selected = selected;
        true
    }

    pub(super) fn clear(&mut self) -> bool {
        let cancelled = self.cancel_interaction();
        let cleared = self.document.clear();
        self.selected.clear();
        cancelled | cleared
    }

    pub(super) fn delete_selection(&mut self) -> bool {
        let selected = std::mem::take(&mut self.selected);
        if selected.is_empty() {
            return false;
        }
        let cancelled = self.cancel_interaction();
        if selected.len() == self.document.elements.len() {
            let elements = std::mem::take(&mut self.document.elements);
            self.document.history.record(HistoryEntry::Clear(elements));
            return true;
        }
        cancelled | self.remove_ids(&selected, false)
    }

    fn remove_ids(&mut self, ids: &[ElementId], merge: bool) -> bool {
        let mut indices: Vec<_> = ids
            .iter()
            .filter_map(|&id| self.document.element_index(id))
            .collect();
        indices.sort_unstable();
        indices.dedup();
        if indices.is_empty() {
            return false;
        }
        let mut next = indices.iter().copied().peekable();
        let mut index = indices[0];
        let end = indices[indices.len() - 1] + 1;
        let removed = self
            .document
            .elements
            .extract_if(index..end, |_| {
                let remove = next.next_if_eq(&index).is_some();
                index += 1;
                remove
            })
            .zip(indices.iter().copied())
            .map(|(element, index)| (index, element))
            .collect();
        let entry = HistoryEntry::Remove(removed);
        if merge {
            self.document.history.merge(entry);
        } else {
            self.document.history.record(entry);
        }
        self.selected
            .retain(|&id| self.document.element_index(id).is_some());
        true
    }

    pub(super) fn insert_kind(&mut self, kind: ElementKind, style: Style) {
        self.insert_element(Element::new(self.next_id, kind, style));
    }

    pub(super) fn insert_element(&mut self, element: Element) {
        self.next_id += 1;
        let index = self.document.elements.len();
        self.document.elements.push(element);
        self.document
            .history
            .record(HistoryEntry::Insert(vec![index]));
    }

    pub(super) fn erase_between(&mut self, start: Point, end: Point) -> bool {
        let radius = self
            .properties(crate::tool::Tool::Eraser)
            .expect("eraser has adjustable properties")
            .size
            * 0.5;
        let hits = self
            .document
            .elements
            .iter()
            .filter(|element| element.erase_hit_test(start, end, radius))
            .map(|element| element.id)
            .collect::<Vec<_>>();
        self.remove_ids(&hits, true)
    }

    pub(super) fn commit_text(&mut self) -> bool {
        let Some(Interaction::EditingText(edit)) = self
            .interaction
            .take_if(|interaction| matches!(interaction, Interaction::EditingText(_)))
        else {
            return false;
        };
        let content = edit.finish();
        let TextEdit {
            id,
            origin,
            style,
            scale,
            ..
        } = edit;
        if content.is_empty() {
            return id.is_none_or(|id| self.remove_ids(&[id], false));
        }
        let kind = ElementKind::Text {
            origin,
            content,
            scale,
        };
        if let Some(id) = id {
            let element = self.element_mut(id).expect("editing text exists");
            if element.kind == kind && element.style == style {
                return true;
            }
            let (kind, style) = element.replace(kind, style);
            self.document
                .history
                .record(HistoryEntry::Update(vec![(id, kind, style)]));
        } else {
            self.insert_kind(kind, style);
        }
        true
    }

    fn begin_text_edit(&mut self, id: ElementId) -> bool {
        let Some(element) = self.element(id) else {
            return false;
        };
        let ElementKind::Text {
            origin,
            content,
            scale,
        } = &element.kind
        else {
            return false;
        };
        let edit = self.make_text_edit(Some(id), *origin, content.clone(), element.style, *scale);
        // Multi-click editing skips cancel_interaction, which normally ends merged undo steps.
        self.document.history.seal();
        self.interaction = Some(Interaction::EditingText(edit));
        true
    }

    pub(super) fn element(&self, id: ElementId) -> Option<&Element> {
        let index = self.document.element_index(id)?;
        Some(&self.document.elements[index])
    }

    pub(super) fn element_mut(&mut self, id: ElementId) -> Option<&mut Element> {
        let index = self.document.element_index(id)?;
        Some(&mut self.document.elements[index])
    }
}
