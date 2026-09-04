use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_keyboard::{KeyState, KeymapFormat, WlKeyboard};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use xkbcommon::xkb;

use super::super::State;
use super::super::draw::{Action, CursorMove, Modifiers};

const KEYCODE_OFFSET: u32 = 8;
const SELECT_ALL_KEY: &str = "a";
const UNDO_KEY: &str = "z";
const REDO_KEY: &str = "y";
const TOGGLE_FILL_KEY: &str = "f";

enum LogicalKey {
    Character(String),
    Escape,
    Delete,
    Backspace,
    Enter,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    Other,
}

struct KeyChord {
    key: LogicalKey,
    modifiers: Modifiers,
}

fn resolve_keybinding(chord: &KeyChord, editing_text: bool) -> Option<Action> {
    use LogicalKey::*;

    if editing_text {
        return match &chord.key {
            Escape => Some(Action::Cancel),
            Delete => Some(Action::Delete),
            Backspace if chord.modifiers.ctrl => Some(Action::BackspaceWord),
            Backspace => Some(Action::Backspace),
            Enter => Some(Action::CommitText),
            ArrowLeft if chord.modifiers.ctrl => Some(Action::MoveCursor(CursorMove::WordLeft)),
            ArrowLeft => Some(Action::MoveCursor(CursorMove::Left)),
            ArrowRight if chord.modifiers.ctrl => Some(Action::MoveCursor(CursorMove::WordRight)),
            ArrowRight => Some(Action::MoveCursor(CursorMove::Right)),
            Home => Some(Action::MoveCursor(CursorMove::Home)),
            End => Some(Action::MoveCursor(CursorMove::End)),
            Character(text) if !chord.modifiers.ctrl && !text.chars().any(char::is_control) => {
                Some(Action::InsertText(text.clone()))
            }
            _ => None,
        };
    }

    match &chord.key {
        Escape => Some(Action::Cancel),
        Delete | Backspace => Some(Action::Delete),
        Character(character)
            if chord.modifiers.ctrl && character.eq_ignore_ascii_case(SELECT_ALL_KEY) =>
        {
            Some(Action::SelectAll)
        }
        Character(character)
            if chord.modifiers.ctrl && character.eq_ignore_ascii_case(REDO_KEY) =>
        {
            Some(Action::Redo)
        }
        Character(character)
            if chord.modifiers.ctrl && character.eq_ignore_ascii_case(UNDO_KEY) =>
        {
            Some(if chord.modifiers.shift {
                Action::Redo
            } else {
                Action::Undo
            })
        }
        Character(character)
            if !chord.modifiers.ctrl && character.eq_ignore_ascii_case(TOGGLE_FILL_KEY) =>
        {
            Some(Action::ToggleFill)
        }
        _ => None,
    }
}

pub(in crate::state) struct KeyboardState {
    context: xkb::Context,
    state: Option<xkb::State>,
    repeat: Option<KeyRepeat>,
    repeat_delay: Duration,
    repeat_interval: Option<Duration>,
}

struct KeyRepeat {
    key: u32,
    next: Instant,
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self {
            context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            state: None,
            repeat: None,
            repeat_delay: Duration::ZERO,
            repeat_interval: None,
        }
    }
}

impl KeyboardState {
    pub(in crate::state) fn clear(&mut self) {
        self.state = None;
        self.repeat = None;
        self.repeat_delay = Duration::ZERO;
        self.repeat_interval = None;
    }

    fn set_keymap(&mut self, fd: OwnedFd, size: u32) -> std::io::Result<()> {
        self.repeat = None;
        self.state = None;
        // SAFETY: Wayland transfers ownership of a valid keymap fd and supplies its mapping size.
        let keymap = unsafe {
            xkb::Keymap::new_from_fd(
                &self.context,
                fd,
                size as usize,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::COMPILE_NO_FLAGS,
            )?
        };
        self.state = keymap.as_ref().map(xkb::State::new);
        Ok(())
    }

    fn update_modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        if let Some(state) = &mut self.state {
            state.update_mask(depressed, latched, locked, 0, 0, group);
        }
    }

    pub(in crate::state) fn modifiers(&self) -> Modifiers {
        let Some(state) = &self.state else {
            return Modifiers::default();
        };
        Modifiers {
            shift: state.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE),
            ctrl: state.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE),
            alt: state.mod_name_is_active(xkb::MOD_NAME_ALT, xkb::STATE_MODS_EFFECTIVE),
        }
    }

    fn chord(&self, evdev_key: u32) -> Option<KeyChord> {
        let state = self.state.as_ref()?;
        let keycode = (evdev_key + KEYCODE_OFFSET).into();
        let keysym = state.key_get_one_sym(keycode);
        let modifiers = self.modifiers();
        let key = match keysym {
            value if value.raw() == xkb::keysyms::KEY_Escape => LogicalKey::Escape,
            value if value.raw() == xkb::keysyms::KEY_Delete => LogicalKey::Delete,
            value if value.raw() == xkb::keysyms::KEY_BackSpace => LogicalKey::Backspace,
            value
                if matches!(
                    value.raw(),
                    xkb::keysyms::KEY_Return | xkb::keysyms::KEY_KP_Enter
                ) =>
            {
                LogicalKey::Enter
            }
            value if value.raw() == xkb::keysyms::KEY_Left => LogicalKey::ArrowLeft,
            value if value.raw() == xkb::keysyms::KEY_Right => LogicalKey::ArrowRight,
            value if value.raw() == xkb::keysyms::KEY_Home => LogicalKey::Home,
            value if value.raw() == xkb::keysyms::KEY_End => LogicalKey::End,
            _ => {
                let text = if modifiers.ctrl {
                    xkb::keysym_to_utf8(keysym)
                } else {
                    state.key_get_utf8(keycode)
                };
                if text.is_empty() {
                    LogicalKey::Other
                } else {
                    LogicalKey::Character(text)
                }
            }
        };
        Some(KeyChord { key, modifiers })
    }

    fn set_repeat_info(&mut self, rate: i32, delay: i32) {
        self.repeat_delay = Duration::from_millis(delay.max(0) as u64);
        self.repeat_interval = (rate > 0)
            .then(|| Duration::from_secs_f64(1.0 / rate as f64).max(Duration::from_millis(1)));
        match (self.repeat.as_mut(), self.repeat_interval) {
            (Some(repeat), Some(interval)) => repeat.next = Instant::now() + interval,
            (Some(_), None) => self.repeat = None,
            _ => {}
        }
    }

    fn update_repeat(&mut self, key: u32, handled: bool) {
        let Some(state) = &self.state else {
            return;
        };
        let keycode = (key + KEYCODE_OFFSET).into();
        if state.get_keymap().key_repeats(keycode) {
            self.repeat = self.repeat_interval.filter(|_| handled).map(|_| KeyRepeat {
                key,
                next: Instant::now() + self.repeat_delay,
            });
        }
    }

    fn stop_repeat(&mut self, key: u32) {
        if self.repeat.as_ref().is_some_and(|repeat| repeat.key == key) {
            self.repeat = None;
        }
    }

    pub(in crate::state) fn cancel_repeat(&mut self) {
        self.repeat = None;
    }

    pub(in crate::state) fn next_wakeup(&self) -> Option<Instant> {
        self.repeat.as_ref().map(|repeat| repeat.next)
    }

    pub(in crate::state) fn repeat_action(
        &mut self,
        now: Instant,
        editing_text: bool,
    ) -> Option<Action> {
        let repeat = self.repeat.as_mut()?;
        if now < repeat.next {
            return None;
        }
        let key = repeat.key;
        repeat.next = now + self.repeat_interval?;
        let chord = self.chord(key)?;
        resolve_keybinding(&chord, editing_text)
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _keyboard: &WlKeyboard,
        event: <WlKeyboard as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_keyboard::Event;

        match event {
            Event::Keymap {
                format: WEnum::Value(KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                if let Err(error) = state.keyboard.set_keymap(fd, size) {
                    eprintln!("vellum: failed to load XKB keymap: {error}");
                }
            }
            Event::Keymap { .. } => state.keyboard.clear(),
            Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                state
                    .keyboard
                    .update_modifiers(mods_depressed, mods_latched, mods_locked, group);
                state.modifiers_changed();
            }
            Event::Leave { .. } => {
                state.keyboard.cancel_repeat();
            }
            Event::RepeatInfo { rate, delay } => state.keyboard.set_repeat_info(rate, delay),
            Event::Key {
                key,
                state: WEnum::Value(KeyState::Pressed),
                ..
            } if state.active => {
                let action = state
                    .keyboard
                    .chord(key)
                    .and_then(|chord| resolve_keybinding(&chord, state.draw.is_editing_text()));
                let repeatable = action
                    .as_ref()
                    .is_some_and(|action| !matches!(action, Action::ToggleFill));
                state.keyboard.update_repeat(key, repeatable);
                if let Some(action) = action {
                    state.apply_action(action);
                }
            }
            Event::Key {
                key,
                state: WEnum::Value(KeyState::Released),
                ..
            } => state.keyboard.stop_repeat(key),
            _ => {}
        }
    }
}
