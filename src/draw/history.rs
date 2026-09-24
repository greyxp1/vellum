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
}

impl History {
    pub(super) fn record(&mut self, entry: Entry) {
        if self.undo.len() == LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(entry);
        self.redo.clear();
    }

    pub(super) fn undo(&mut self, elements: &mut Vec<Element>) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        self.redo.push(apply(entry, elements));
        true
    }

    pub(super) fn redo(&mut self, elements: &mut Vec<Element>) -> bool {
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
