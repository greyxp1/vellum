use std::{borrow::Cow, mem};

use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    Action as ProtocolAction, ChangeCause, ContentHint, ContentPurpose, Event,
    PreeditHint as ProtocolPreeditHint, ZwpTextInputV3,
};

use crate::OutputId;
use crate::draw::{Action, Preedit, PreeditHint, PreeditSpan, TextInputBatch, TextInputSnapshot};
use crate::state::State;

const MAX_SURROUNDING_BYTES: usize = 4000;

#[derive(Default)]
struct TextInputSession {
    id: u64,
    enable_serial: u32,
    waiting_for_matching_done: bool,
    surrounding: (String, [i32; 2]),
    external_revision: u64,
    rectangle: [i32; 4],
}

#[derive(Default)]
pub(in crate::state) struct TextInputState {
    focused_output: Option<OutputId>,
    serial: u32,
    pending: TextInputBatch,
    hints: Vec<PreeditSpan>,
    session: Option<TextInputSession>,
}

impl TextInputState {
    fn reset(&mut self) {
        self.session = None;
        self.pending = TextInputBatch::default();
        self.hints.clear();
    }

    fn commit(&mut self, text_input: &ZwpTextInputV3) {
        text_input.commit();
        self.serial = self.serial.wrapping_add(1);
    }

    pub(in crate::state) fn sync(
        &mut self,
        text_input: &ZwpTextInputV3,
        snapshot: Option<TextInputSnapshot<'_>>,
    ) {
        let snapshot = snapshot.filter(|_| self.focused_output.is_some());
        if self.session.as_ref().map(|session| session.id)
            != snapshot.map(|snapshot| snapshot.session)
        {
            if self.session.is_some() {
                if text_input.version() >= 2 {
                    text_input.hide_input_panel();
                }
                text_input.disable();
                self.commit(text_input);
            }
            self.reset();
        }
        let Some(snapshot) = snapshot else { return };
        let Some(rectangle) = snapshot.cursor_rectangle else {
            return;
        };
        let previous = self.session.as_ref();
        if previous.is_some_and(|session| session.waiting_for_matching_done) {
            return;
        }

        let (text, positions) =
            surrounding_text(snapshot.content, snapshot.cursor, snapshot.anchor);
        let external =
            previous.is_none_or(|sent| sent.external_revision != snapshot.external_revision);
        let surrounding_changed = previous
            .is_none_or(|sent| sent.surrounding.0 != text || sent.surrounding.1 != positions);
        let rectangle_changed = previous.is_none_or(|sent| sent.rectangle != rectangle);
        if !external && !surrounding_changed && !rectangle_changed {
            return;
        }

        if self.session.is_none() {
            text_input.enable();
            let hint = if text_input.version() >= 2 {
                text_input
                    .set_available_actions((ProtocolAction::Submit as u32).to_ne_bytes().to_vec());
                text_input.show_input_panel();
                ContentHint::PreeditShown
            } else {
                ContentHint::None
            };
            text_input.set_content_type(hint, ContentPurpose::Normal);
        }
        if surrounding_changed {
            text_input.set_surrounding_text(text.to_string(), positions[0], positions[1]);
        }
        if external {
            text_input.set_text_change_cause(ChangeCause::Other);
        }
        if rectangle_changed {
            text_input.set_cursor_rectangle(rectangle[0], rectangle[1], rectangle[2], rectangle[3]);
        }
        self.commit(text_input);
        let sent = self.session.get_or_insert_with(|| TextInputSession {
            id: snapshot.session,
            enable_serial: self.serial,
            ..Default::default()
        });
        if surrounding_changed {
            let (sent_text, sent_positions) = &mut sent.surrounding;
            match text {
                Cow::Borrowed(text) => text.clone_into(sent_text),
                Cow::Owned(text) => *sent_text = text,
            }
            *sent_positions = positions;
        }
        sent.external_revision = snapshot.external_revision;
        sent.rectangle = rectangle;
    }

    pub(in crate::state) fn sync_render(
        &mut self,
        proxy: Option<&ZwpTextInputV3>,
        output: OutputId,
        snapshot: Option<TextInputSnapshot<'_>>,
    ) {
        if self.focused_output == Some(output)
            && let Some(proxy) = proxy
        {
            self.sync(proxy, snapshot);
        }
    }
}

impl State {
    pub(in crate::state) fn text_input_output_removed(&mut self, output: OutputId) {
        if self.text_input.focused_output == Some(output) {
            self.text_input.focused_output = None;
            self.text_input.reset();
            if self.draw.clear_preedit() {
                self.request_render();
            }
        }
    }

    pub(crate) fn sync_text_input(&mut self) {
        if let Some(proxy) = &self.wayland.text_input {
            self.text_input.sync(proxy, self.draw.text_input_snapshot());
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn event(
        state: &mut Self,
        _text_input: &ZwpTextInputV3,
        event: Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            Event::Enter { surface } => {
                state.text_input.reset();
                state.text_input.focused_output = state.output_for_surface(&surface);
                if let Some(output) = state.text_input.focused_output {
                    state.draw.damage(output);
                    state.request_render();
                }
            }
            Event::Leave { surface } => {
                if let Some(output) = surface.data::<OutputId>() {
                    state.text_input_output_removed(*output);
                }
            }
            Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                state.text_input.pending.preedit =
                    text.filter(|text| !text.is_empty()).map(|text| {
                        let cursor = preedit_cursor(&text, cursor_begin, cursor_end);
                        Preedit {
                            text,
                            cursor,
                            spans: Vec::new(),
                        }
                    });
            }
            Event::CommitString { text } => state.text_input.pending.commit = text,
            Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                state.text_input.pending.delete_surrounding =
                    Some((before_length as usize, after_length as usize));
            }
            Event::Done { serial } => {
                let mut batch = mem::take(&mut state.text_input.pending);
                if let Some(preedit) = &mut batch.preedit {
                    preedit.spans.extend(
                        state
                            .text_input
                            .hints
                            .drain(..)
                            .filter(|span| valid_range(&preedit.text, &span.range)),
                    );
                }
                state.text_input.hints.clear();
                let Some(session) = &mut state.text_input.session else {
                    return;
                };
                // A reply from a previous enable must never edit the new field.
                if Some(session.id) != state.draw.text_input_snapshot().map(|s| s.session)
                    || (serial.wrapping_sub(session.enable_serial) as i32) < 0
                {
                    return;
                }
                let resume = session.waiting_for_matching_done && serial == state.text_input.serial;
                session.waiting_for_matching_done = serial != state.text_input.serial;
                if resume && let Some(output) = state.text_input.focused_output {
                    state.draw.damage(output);
                }
                state.apply_action(Action::ApplyTextInput(batch));
                if resume {
                    state.request_render();
                }
            }
            Event::Action {
                action: WEnum::Value(ProtocolAction::Submit),
                ..
            } => {
                state.text_input.pending.submit = true;
            }
            Event::PreeditHint {
                start,
                end,
                hint: WEnum::Value(hint),
            } => {
                state.text_input.hints.push(PreeditSpan {
                    range: start as usize..end as usize,
                    style: match hint {
                        ProtocolPreeditHint::Selection => PreeditHint::Selection,
                        ProtocolPreeditHint::SpellingError => PreeditHint::SpellingError,
                        ProtocolPreeditHint::ComposeError => PreeditHint::ComposeError,
                        ProtocolPreeditHint::Prediction => PreeditHint::Prediction,
                        _ => PreeditHint::Whole,
                    },
                });
            }
            // Language is informational; Vellum does not choose an IME language.
            _ => {}
        }
    }
}

fn surrounding_text(
    text: parley::SplitString<'_>,
    cursor: usize,
    anchor: usize,
) -> (Cow<'_, str>, [i32; 2]) {
    let mut parts = text.into_iter();
    let (before, after) = (parts.next().unwrap(), parts.next().unwrap());
    // Move the anchor of a selection that cannot fit toward the cursor.
    let anchor = if anchor > cursor {
        before.floor_char_boundary(anchor.min(cursor + MAX_SURROUNDING_BYTES))
    } else {
        before.ceil_char_boundary(anchor.max(cursor.saturating_sub(MAX_SURROUNDING_BYTES)))
    };
    let room = MAX_SURROUNDING_BYTES - cursor.abs_diff(anchor);
    let start = before.ceil_char_boundary(cursor.min(anchor).saturating_sub(room / 2));
    if after.is_empty() {
        let end = before.floor_char_boundary((start + MAX_SURROUNDING_BYTES).min(before.len()));
        let start = before.ceil_char_boundary(end.saturating_sub(MAX_SURROUNDING_BYTES));
        return (
            Cow::Borrowed(&before[start..end]),
            [(cursor - start) as i32, (anchor - start) as i32],
        );
    }
    // While composing, Parley splits committed text at the preedit's insertion point.
    let end =
        after.floor_char_boundary((MAX_SURROUNDING_BYTES - (cursor - start)).min(after.len()));
    let start = before.ceil_char_boundary(cursor.saturating_sub(MAX_SURROUNDING_BYTES - end));
    (
        Cow::Owned([&before[start..], &after[..end]].concat()),
        [(cursor - start) as i32, (anchor - start) as i32],
    )
}

fn preedit_cursor(text: &str, begin: i32, end: i32) -> Option<(usize, usize)> {
    let (Ok(begin), Ok(end)) = (usize::try_from(begin), usize::try_from(end)) else {
        return None;
    };
    (text.is_char_boundary(begin) && text.is_char_boundary(end)).then_some((begin, end))
}

fn valid_range(text: &str, range: &std::ops::Range<usize>) -> bool {
    range.start <= range.end
        && text.is_char_boundary(range.start)
        && text.is_char_boundary(range.end)
}
