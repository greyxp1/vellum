use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_keyboard::{KeyState, KeymapFormat, WlKeyboard};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use xkbcommon::xkb;

use crate::config::{BindingAction, Keybinds, MouseButton};
use crate::draw::{Action, Modifiers};
use crate::state::State;

const KEYCODE_OFFSET: u32 = 8;

struct KeyChord {
    binding: Option<BindingAction>,
    text: String,
    shift: bool,
    composed: bool,
}

impl KeyChord {
    fn into_action(self) -> Option<Action> {
        if let Some(binding) = self.binding {
            return binding.action(self.shift);
        }
        if !self.text.is_empty() && !self.text.chars().any(char::is_control) {
            return Some(Action::InsertText(self.text));
        }
        None
    }
}

pub(in crate::state) struct KeyboardState {
    keybinds: Keybinds,
    text_keybinds: Keybinds,
    context: xkb::Context,
    state: Option<xkb::State>,
    shift_free_state: Option<xkb::State>,
    modifiers: Modifiers,
    logo: bool,
    compose: Option<xkb::compose::State>,
    compose_session: Option<u64>,
    repeat: Option<KeyRepeat>,
    repeat_delay: Duration,
    repeat_interval: Option<Duration>,
}

struct KeyRepeat {
    key: u32,
    composed_text: Option<String>,
    next: Instant,
}

impl KeyboardState {
    pub(in crate::state) fn mouse_binding(
        &self,
        button: MouseButton,
        editing_text: bool,
        modifiers: Modifiers,
        logo: bool,
    ) -> Option<BindingAction> {
        if editing_text {
            self.text_keybinds
                .mouse_binding(button, modifiers, logo, false)
                .or_else(|| self.keybinds.mouse_binding(button, modifiers, logo, true))
        } else {
            self.keybinds.mouse_binding(button, modifiers, logo, false)
        }
    }

    pub(in crate::state) fn logo(&self) -> bool {
        self.logo
    }

    pub(in crate::state) fn reload(&mut self, keybinds: Keybinds, text_keybinds: Keybinds) {
        self.cancel_repeat();
        self.reset_compose();
        self.keybinds = keybinds;
        self.text_keybinds = text_keybinds;
    }

    pub(in crate::state) fn new(keybinds: Keybinds, text_keybinds: Keybinds) -> Self {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .filter_map(std::env::var_os)
            .find(|locale| !locale.is_empty())
            .unwrap_or_else(|| "C".into());
        let compose =
            xkb::compose::Table::new_from_locale(&context, &locale, xkb::compose::COMPILE_NO_FLAGS)
                .ok()
                .map(|table| xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS));
        Self {
            keybinds,
            text_keybinds,
            context,
            compose,
            compose_session: None,
            state: None,
            shift_free_state: None,
            modifiers: Modifiers::default(),
            logo: false,
            repeat: None,
            repeat_delay: Duration::ZERO,
            repeat_interval: None,
        }
    }

    pub(in crate::state) fn clear(&mut self) {
        self.clear_keymap();
        self.repeat_delay = Duration::ZERO;
        self.repeat_interval = None;
    }

    fn clear_keymap(&mut self) {
        self.reset_compose();
        self.repeat = None;
        self.state = None;
        self.shift_free_state = None;
        self.modifiers = Modifiers::default();
        self.logo = false;
    }

    fn set_keymap(&mut self, fd: OwnedFd, size: u32) -> std::io::Result<()> {
        self.clear_keymap();
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
        self.shift_free_state = keymap.as_ref().map(xkb::State::new);
        Ok(())
    }

    fn update_modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        if let Some(state) = &mut self.state {
            state.update_mask(depressed, latched, locked, 0, 0, group);
            let active = |name| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
            self.modifiers = Modifiers {
                shift: active(xkb::MOD_NAME_SHIFT),
                ctrl: active(xkb::MOD_NAME_CTRL),
                alt: active(xkb::MOD_NAME_ALT),
            };
            self.logo = active(xkb::MOD_NAME_LOGO);
        }
        if let Some(state) = &mut self.shift_free_state {
            // Preserve other modifiers and the active layout when matching Shift shortcuts.
            let shift = state.get_keymap().mod_get_index(xkb::MOD_NAME_SHIFT);
            let mask = !1u32.checked_shl(shift).unwrap_or(0);
            state.update_mask(depressed & mask, latched & mask, locked & mask, 0, 0, group);
        }
    }

    pub(in crate::state) fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    fn chord(
        &mut self,
        evdev_key: u32,
        editing_text: bool,
        compose_enabled: bool,
    ) -> Option<KeyChord> {
        let state = self.state.as_ref()?;
        let keycode = (evdev_key + KEYCODE_OFFSET).into();
        let keysym = state.key_get_one_sym(keycode);
        let (modifiers, logo) = (self.modifiers, self.logo);
        let bindings = if editing_text {
            &self.text_keybinds
        } else {
            &self.keybinds
        };
        let shift_free_key = modifiers
            .shift
            .then(|| {
                self.shift_free_state
                    .as_ref()
                    .map(|state| state.key_get_one_sym(keycode).raw())
            })
            .flatten();
        let binding = bindings
            .lookup(keysym.raw(), shift_free_key, modifiers, logo, false)
            .or_else(|| {
                if !editing_text {
                    return None;
                }
                // Keys that type text are not gestures while editing.
                let text = state.key_get_utf8(keycode);
                if !(modifiers.ctrl || modifiers.alt || logo)
                    && !text.is_empty()
                    && !text.chars().any(char::is_control)
                {
                    return None;
                }
                self.keybinds
                    .lookup(keysym.raw(), shift_free_key, modifiers, logo, true)
            });
        let mut composed = false;
        let text = if binding.is_some() || !editing_text || modifiers.ctrl || modifiers.alt || logo
        {
            self.reset_compose();
            String::new()
        } else {
            let mut text = state.key_get_utf8(keycode);
            if compose_enabled && let Some(compose) = &mut self.compose {
                compose.feed(keysym);
                match compose.status() {
                    xkb::compose::Status::Composing => text.clear(),
                    xkb::compose::Status::Cancelled => {
                        compose.reset();
                        text.clear();
                    }
                    xkb::compose::Status::Composed => {
                        text = compose.utf8().unwrap_or_default();
                        compose.reset();
                        composed = true;
                    }
                    xkb::compose::Status::Nothing => {}
                }
            }
            text
        };
        Some(KeyChord {
            binding,
            text,
            shift: modifiers.shift,
            composed,
        })
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

    fn update_repeat(&mut self, key: u32, handled: bool, composed_text: Option<String>) {
        if self
            .compose
            .as_ref()
            .is_some_and(|compose| compose.status() == xkb::compose::Status::Composing)
        {
            self.repeat = None;
            return;
        }
        let Some(state) = &self.state else {
            return;
        };
        let keycode = (key + KEYCODE_OFFSET).into();
        if state.get_keymap().key_repeats(keycode) {
            self.repeat = self.repeat_interval.filter(|_| handled).map(|_| KeyRepeat {
                key,
                composed_text,
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
        self.reset_compose();
    }

    pub(in crate::state) fn reset_compose(&mut self) {
        if let Some(compose) = &mut self.compose {
            compose.reset();
        }
    }

    fn sync_text_session(&mut self, session: Option<u64>) {
        if self.compose_session != session {
            self.cancel_repeat();
            self.compose_session = session;
        }
    }

    pub(in crate::state) fn next_wakeup(&self) -> Option<Instant> {
        self.repeat.as_ref().map(|repeat| repeat.next)
    }

    pub(in crate::state) fn repeat_action(
        &mut self,
        now: Instant,
        text_session: Option<u64>,
    ) -> Option<Action> {
        self.sync_text_session(text_session);
        let repeat = self.repeat.as_mut()?;
        if now < repeat.next {
            return None;
        }
        let key = repeat.key;
        let composed_text = repeat.composed_text.clone();
        repeat.next = now + self.repeat_interval?;
        let mut chord = self.chord(key, text_session.is_some(), false)?;
        if chord.binding.is_some_and(|binding| !binding.repeatable()) {
            self.cancel_repeat();
            return None;
        }
        if chord.binding.is_none()
            && !chord.text.is_empty()
            && let Some(text) = composed_text
        {
            chord.text = text;
        }
        chord.into_action()
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
                state.release_keyboard_gestures();
                if let Err(error) = state.keyboard.set_keymap(fd, size) {
                    log::error!("failed to load XKB keymap: {error}");
                }
            }
            Event::Keymap { .. } => {
                state.release_keyboard_gestures();
                state.keyboard.clear_keymap();
            }
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
                state.release_keyboard_gestures();
                state.refresh_cursor();
            }
            Event::RepeatInfo { rate, delay } => state.keyboard.set_repeat_info(rate, delay),
            Event::Key {
                key,
                state: WEnum::Value(KeyState::Pressed),
                time,
                ..
            } if state.is_active() => {
                let session = state
                    .draw
                    .text_input_snapshot()
                    .map(|snapshot| snapshot.session);
                state.keyboard.sync_text_session(session);
                let editing = state.draw.is_editing_text();
                let chord = state.keyboard.chord(key, editing, true);
                let binding = chord.as_ref().and_then(|chord| chord.binding);
                if state.keyboard_gesture(key, binding, true, time) {
                    state.keyboard.cancel_repeat();
                    return;
                }
                if binding == Some(BindingAction::ToggleWhiteboard) {
                    state.keyboard.cancel_repeat();
                    state.toggle_whiteboard();
                    return;
                }
                if binding == Some(BindingAction::ToggleFreeze) {
                    state.keyboard.cancel_repeat();
                    state.toggle_freeze();
                    return;
                }
                if state.freeze.capturing() {
                    state.keyboard.cancel_repeat();
                    if binding == Some(BindingAction::Cancel) {
                        state.apply_action(Action::Cancel);
                    }
                    return;
                }
                let composed_text = chord
                    .as_ref()
                    .filter(|chord| chord.composed && !chord.text.is_empty())
                    .map(|chord| chord.text.clone());
                let action = chord.and_then(KeyChord::into_action);
                let repeatable = action.is_some() && binding.is_none_or(BindingAction::repeatable);
                state.keyboard.update_repeat(key, repeatable, composed_text);
                if let Some(action) = action {
                    state.apply_action(action);
                }
            }
            Event::Key {
                key,
                state: WEnum::Value(KeyState::Released),
                time,
                ..
            } => {
                state.keyboard.stop_repeat(key);
                state.keyboard_gesture(key, None, false, time);
            }
            _ => {}
        }
    }
}
