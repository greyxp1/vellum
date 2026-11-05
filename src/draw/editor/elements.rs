use std::time::{Duration, Instant};

use super::{Editor, HistoryEntry, Interaction};
use crate::draw::scene::{Element, ElementId, ElementKind, Point, Style};
use crate::draw::text_edit::TextEdit;

impl Editor {
    fn protected_from_fading(&self, id: ElementId) -> bool {
        self.selected.contains(&id) || self.text_edit().is_some_and(|edit| edit.id == Some(id))
    }

    pub(in crate::draw) fn annotation_remaining(&self, element: &Element, now: Instant) -> f32 {
        element.remaining(
            now,
            self.presentation_settings,
            self.protected_from_fading(element.id),
        )
    }

    pub(in crate::draw) fn next_fade(&self, now: Instant) -> Option<Instant> {
        [&self.document, &self.other_document]
            .into_iter()
            .flat_map(|document| &document.elements)
            .filter(|element| !self.protected_from_fading(element.id))
            .filter_map(|element| element.fade_started_at)
            .map(|started| {
                let start = started + self.presentation_settings.hold;
                if start > now {
                    start
                } else {
                    now + Duration::from_millis(16)
                }
            })
            .min()
    }

    pub(in crate::draw) fn expire_annotations(&mut self, now: Instant) {
        let mut expired: Vec<_> = [&self.document, &self.other_document]
            .into_iter()
            .flat_map(|document| &document.elements)
            .filter(|element| self.annotation_remaining(element, now) <= 0.0)
            .map(|element| element.id)
            .collect();
        if expired.is_empty() {
            return;
        }
        expired.sort_unstable();
        for document in [&mut self.document, &mut self.other_document] {
            let previous_len = document.elements.len();
            document
                .elements
                .retain(|element| expired.binary_search(&element.id).is_err());
            if document.elements.len() != previous_len {
                document.history.forget(&expired);
            }
        }
    }

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
        if matches!(element.kind, ElementKind::Text { .. })
            && element.hit_test(point, 1.0, self.presentation_settings)
        {
            let changed = self.begin_text_edit(id);
            if let Some(edit) = self.text_edit_mut() {
                edit.click(point, clicks, false);
            }
            return Some(changed);
        }
        None
    }

    pub(in crate::draw) fn hit_test(&self, point: Point) -> Option<ElementId> {
        let now = Instant::now();
        self.document
            .elements
            .iter()
            .rev()
            .find(|element| {
                element.hit_test(
                    point,
                    self.annotation_remaining(element, now),
                    self.presentation_settings,
                )
            })
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
        let mut selected = std::mem::take(&mut self.selected);
        if selected.is_empty() {
            return false;
        }
        let cancelled = self.cancel_interaction();
        if selected.len() == self.document.elements.len() {
            let elements = std::mem::take(&mut self.document.elements);
            self.document.history.record(HistoryEntry::Clear(elements));
            return true;
        }
        selected.sort_unstable();
        let start = self.document.element_index(selected[0]).unwrap();
        let end = self
            .document
            .element_index(*selected.last().unwrap())
            .unwrap();
        cancelled
            | self.remove_elements(
                start..=end,
                |element| selected.binary_search(&element.id).is_ok(),
                false,
            )
    }

    fn remove_elements(
        &mut self,
        range: impl std::ops::RangeBounds<usize>,
        mut remove: impl FnMut(&Element) -> bool,
        merge: bool,
    ) -> bool {
        let removed: Vec<_> = self
            .document
            .elements
            .extract_if(range, |element| remove(element))
            .collect();
        if removed.is_empty() {
            return false;
        }
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

    pub(super) fn insert_element(&mut self, mut element: Element) {
        element.fade_started_at = self.presentation.then(Instant::now);
        self.next_id += 1;
        let id = element.id;
        self.document.elements.push(element);
        self.document.history.record(HistoryEntry::Insert(vec![id]));
    }

    pub(super) fn erase_between(&mut self, start: Point, end: Point, size: f32) -> bool {
        let radius = size * 0.5;
        let now = Instant::now();
        let settings = self.presentation_settings;
        let selected = self.selected.clone();
        self.remove_elements(
            ..,
            |element| {
                let remaining = element.remaining(now, settings, selected.contains(&element.id));
                element.erase_hit_test(start, end, radius, remaining, settings)
            },
            true,
        )
    }

    pub(super) fn commit_text(&mut self) -> bool {
        let Some(Interaction::EditingText(edit)) = self
            .interaction
            .take_if(|interaction| matches!(interaction, Interaction::EditingText(_)))
        else {
            return false;
        };
        let content = edit.finish();
        if content.is_empty() {
            return edit.id.is_none_or(|id| {
                self.document
                    .element_index(id)
                    .is_some_and(|index| self.remove_elements(index..=index, |_| true, false))
            });
        }
        let text = crate::text::Text::from_layout(
            content,
            edit.font.clone(),
            edit.style.size,
            edit.layout().clone(),
        );
        let TextEdit {
            id,
            origin,
            style,
            scale,
            ..
        } = edit;
        let kind = ElementKind::Text {
            origin,
            text,
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
            text,
            scale,
        } = &element.kind
        else {
            return false;
        };
        let edit = self.make_text_edit(
            Some(id),
            *origin,
            text.content.to_string(),
            element.style,
            *scale,
            text.font.clone(),
        );
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
