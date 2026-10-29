use super::scene::{Element, ElementId, ElementKind, Style};

const LIMIT: usize = 256;

pub(super) enum Entry {
    Insert(Vec<usize>),
    Remove(Vec<(usize, Element)>),
    Update(Vec<(ElementId, ElementKind, Style)>),
    Clear(Vec<Element>),
}

#[derive(Default)]
pub(super) struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    merging: bool,
}

impl History {
    pub(super) fn refresh_text_bounds(&mut self) {
        for entry in self.undo.iter_mut().chain(&mut self.redo) {
            match entry {
                Entry::Remove(elements) => {
                    for (_, element) in elements {
                        element.refresh_text_bounds();
                    }
                }
                Entry::Clear(elements) => {
                    for element in elements {
                        element.refresh_text_bounds();
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) fn record(&mut self, entry: Entry) {
        self.merging = false;
        self.push(entry);
    }

    /// Folds consecutive merged entries of the same change into one undo step.
    pub(super) fn merge(&mut self, entry: Entry) {
        let merging = std::mem::replace(&mut self.merging, true);
        match (self.undo.last_mut(), entry) {
            (Some(Entry::Remove(previous)), Entry::Remove(mut removed)) if merging => {
                // Undo reinserts sequentially, so later removals come first.
                removed.append(previous);
                *previous = removed;
            }
            (Some(Entry::Update(previous)), Entry::Update(updates)) if merging => {
                // Keep each element's state from before its first merged change.
                for update in updates {
                    if !previous.iter().any(|(id, ..)| *id == update.0) {
                        previous.push(update);
                    }
                }
            }
            (_, entry) => self.push(entry),
        }
    }

    /// Makes the next merged entry start a new undo step.
    pub(super) fn seal(&mut self) {
        self.merging = false;
    }

    fn push(&mut self, entry: Entry) {
        if self.undo.len() == LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(entry);
        self.redo.clear();
    }

    pub(super) fn undo(&mut self, elements: &mut Vec<Element>) -> bool {
        self.merging = false;
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        self.redo.push(apply(entry, elements));
        true
    }

    pub(super) fn redo(&mut self, elements: &mut Vec<Element>) -> bool {
        self.merging = false;
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        self.undo.push(apply(entry, elements));
        true
    }
}

fn apply(entry: Entry, elements: &mut Vec<Element>) -> Entry {
    match entry {
        Entry::Insert(inserted) => {
            let mut removed = Vec::with_capacity(inserted.len());
            for index in inserted.into_iter().rev() {
                removed.push((index, elements.remove(index)));
            }
            removed.reverse();
            Entry::Remove(removed)
        }
        Entry::Remove(removed) => {
            let mut inserted = Vec::with_capacity(removed.len());
            for (index, element) in removed {
                elements.insert(index, element);
                inserted.push(index);
            }
            Entry::Insert(inserted)
        }
        Entry::Update(updates) => Entry::Update(
            updates
                .into_iter()
                .map(|(id, kind, style)| {
                    let index = elements
                        .binary_search_by_key(&id, |element| element.id)
                        .expect("history element exists");
                    let element = &mut elements[index];
                    let (kind, style) = element.replace(kind, style);
                    (id, kind, style)
                })
                .collect(),
        ),
        Entry::Clear(mut previous) => {
            std::mem::swap(elements, &mut previous);
            Entry::Clear(previous)
        }
    }
}
