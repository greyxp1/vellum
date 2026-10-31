use super::scene::{Element, ElementId, ElementKind, Style};

const LIMIT: usize = 256;

pub(super) enum Entry {
    Insert(Vec<ElementId>),
    Remove(Vec<Element>),
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
                Entry::Remove(elements) | Entry::Clear(elements) => {
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
                previous.append(&mut removed);
            }
            (Some(Entry::Update(previous)), Entry::Update(updates)) if merging => {
                // Keep each element's state from before its first merged change.
                previous.extend(updates);
                previous.sort_by_key(|(id, ..)| *id);
                previous.dedup_by_key(|(id, ..)| *id);
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
        Entry::Insert(inserted) if elements.last().is_some_and(|last| inserted == [last.id]) => {
            Entry::Remove(vec![elements.pop().unwrap()])
        }
        Entry::Insert(mut inserted) => {
            inserted.sort_unstable();
            let start = elements.partition_point(|element| element.id < inserted[0]);
            let end = elements.partition_point(|element| element.id <= *inserted.last().unwrap());
            Entry::Remove(
                elements
                    .extract_if(start..end, |element| {
                        inserted.binary_search(&element.id).is_ok()
                    })
                    .collect(),
            )
        }
        Entry::Remove(removed) => {
            let inserted: Vec<_> = removed.iter().map(|element| element.id).collect();
            let first = *inserted.iter().min().unwrap();
            let start = elements.partition_point(|element| element.id < first);
            elements.extend(removed);
            // IDs preserve drawing order across eraser motions and canvas switches.
            elements[start..].sort_unstable_by_key(|element| element.id);
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
