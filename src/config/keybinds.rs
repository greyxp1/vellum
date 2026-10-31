use std::collections::BTreeMap;

use xkbcommon::xkb;

use crate::draw::{Action, CursorMove, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BindingAction {
    Draw,
    OpenPicker,
    Erase,
    Undo,
    Redo,
    SelectAll,
    Delete,
    Cancel,
    Clear,
    ToggleFill,
    ToggleEraser,
    ToggleWhiteboard,
    ToggleFreeze,
    CommitText,
    Newline,
    Backspace,
    BackspaceWord,
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    TextStart,
    TextEnd,
}

impl BindingAction {
    pub(crate) fn pointer_gesture(self) -> bool {
        matches!(self, Self::Draw | Self::OpenPicker | Self::Erase)
    }

    pub(crate) fn repeatable(self) -> bool {
        !matches!(
            self,
            Self::ToggleFill | Self::ToggleEraser | Self::ToggleWhiteboard | Self::ToggleFreeze
        )
    }

    fn movement(self) -> Option<CursorMove> {
        Some(match self {
            Self::Left => CursorMove::Left,
            Self::Right => CursorMove::Right,
            Self::Up => CursorMove::Up,
            Self::Down => CursorMove::Down,
            Self::WordLeft => CursorMove::WordLeft,
            Self::WordRight => CursorMove::WordRight,
            Self::Home => CursorMove::Home,
            Self::End => CursorMove::End,
            Self::TextStart => CursorMove::TextStart,
            Self::TextEnd => CursorMove::TextEnd,
            _ => return None,
        })
    }

    pub(crate) fn action(self, shift: bool) -> Option<Action> {
        if let Some(movement) = self.movement() {
            return Some(Action::MoveCursor(movement, shift));
        }
        Some(match self {
            Self::Undo => Action::Undo,
            Self::Redo => Action::Redo,
            Self::SelectAll => Action::SelectAll,
            Self::Delete => Action::Delete,
            Self::Cancel => Action::Cancel,
            Self::Clear => Action::Clear,
            Self::ToggleFill => Action::ToggleFill,
            Self::ToggleEraser => Action::ToggleEraser,
            Self::CommitText => Action::CommitText,
            Self::Newline => Action::InsertText("\n".into()),
            Self::Backspace => Action::Backspace,
            Self::BackspaceWord => Action::BackspaceWord,
            _ => return None,
        })
    }
}

pub(super) type KeybindConfig = BTreeMap<BindingAction, Vec<String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub(crate) const ALL: [Self; 5] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Back,
        Self::Forward,
    ];

    pub(crate) fn from_evdev(button: u32) -> Option<Self> {
        match button {
            272 => Some(Self::Left),
            273 => Some(Self::Right),
            274 => Some(Self::Middle),
            275 | 278 => Some(Self::Back),
            276 | 277 => Some(Self::Forward),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Left => "MouseLeft",
            Self::Right => "MouseRight",
            Self::Middle => "MouseMiddle",
            Self::Back => "MouseBack",
            Self::Forward => "MouseForward",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|button| name.eq_ignore_ascii_case(button.name()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Input {
    Keyboard(u32),
    Mouse(MouseButton),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Keybind {
    key: Input,
    modifiers: Modifiers,
    logo: bool,
}

impl std::fmt::Display for Keybind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (active, name) in [
            (self.modifiers.ctrl, "Ctrl"),
            (self.modifiers.shift, "Shift"),
            (self.modifiers.alt, "Alt"),
            (self.logo, "Super"),
        ] {
            if active {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            Input::Keyboard(key) => f.write_str(&xkb::keysym_get_name(key.into())),
            Input::Mouse(button) => f.write_str(button.name()),
        }
    }
}

impl Keybind {
    fn parse(value: &str) -> Result<Self, String> {
        let mut parts = value.rsplit('+').map(str::trim);
        let name = parts.next().unwrap_or_default();
        if name.is_empty() || name.contains('\0') {
            return Err("expected an XKB key name or mouse button".into());
        }
        let name = if name.eq_ignore_ascii_case("enter") {
            "Return"
        } else if name.eq_ignore_ascii_case("esc") {
            "Escape"
        } else {
            name
        };
        let key = if let Some(button) = MouseButton::parse(name) {
            Input::Mouse(button)
        } else {
            let key = xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE).raw();
            if key == xkb::keysyms::KEY_NoSymbol {
                return Err(format!("unknown key or mouse button {name:?}"));
            }
            Input::Keyboard(key)
        };
        let mut binding = Self {
            key,
            modifiers: Modifiers::default(),
            logo: false,
        };
        for modifier in parts {
            let flag = match modifier.to_ascii_lowercase().as_str() {
                "ctrl" => &mut binding.modifiers.ctrl,
                "shift" => &mut binding.modifiers.shift,
                "alt" => &mut binding.modifiers.alt,
                "super" => &mut binding.logo,
                _ => return Err(format!("unknown modifier {modifier:?}")),
            };
            if *flag {
                return Err(format!("duplicate modifier {modifier:?}"));
            }
            *flag = true;
        }
        Ok(binding)
    }
}

#[derive(Clone)]
struct ResolvedKeybind {
    key: Keybind,
    action: BindingAction,
    automatic_selection: bool,
}

#[derive(Clone)]
pub(crate) struct Keybinds(Vec<ResolvedKeybind>);

impl Keybinds {
    pub(super) fn resolve(config: KeybindConfig, text: bool) -> Result<Self, String> {
        use BindingAction::*;
        let defaults: &[(BindingAction, &[&str])] = if text {
            &[
                (Cancel, &["Escape"]),
                (Delete, &["Delete"]),
                (Backspace, &["BackSpace"]),
                (BackspaceWord, &["Ctrl+BackSpace"]),
                (CommitText, &["Return", "KP_Enter"]),
                (Newline, &["Shift+Return", "Shift+KP_Enter"]),
                (SelectAll, &["Ctrl+a"]),
                (Left, &["Left"]),
                (Right, &["Right"]),
                (Up, &["Up"]),
                (Down, &["Down"]),
                (WordLeft, &["Ctrl+Left"]),
                (WordRight, &["Ctrl+Right"]),
                (Home, &["Home"]),
                (End, &["End"]),
                (TextStart, &["Ctrl+Home"]),
                (TextEnd, &["Ctrl+End"]),
            ]
        } else {
            &[
                (Draw, &["MouseLeft"]),
                (OpenPicker, &["MouseRight"]),
                (Erase, &["MouseMiddle"]),
                (Cancel, &["Escape"]),
                (Delete, &["Delete", "BackSpace"]),
                (SelectAll, &["Ctrl+a"]),
                (Undo, &["Ctrl+z", "MouseBack"]),
                (Redo, &["Ctrl+Shift+z", "Ctrl+y", "MouseForward"]),
                (ToggleFill, &["f"]),
                (ToggleWhiteboard, &["w"]),
                (ToggleFreeze, &["space"]),
            ]
        };
        let mut configured = defaults
            .iter()
            .map(|(action, keys)| (*action, keys.iter().map(|key| (*key).to_owned()).collect()))
            .collect::<KeybindConfig>();
        configured.extend(config);
        let table = if text { "text_keybinds" } else { "keybinds" };
        let mut bindings: Vec<ResolvedKeybind> = Vec::new();
        for (action, keys) in configured {
            let text_action = action.movement().is_some()
                || matches!(action, CommitText | Newline | Backspace | BackspaceWord);
            let shared_action = matches!(action, Cancel | Delete | SelectAll);
            if !shared_action && text_action != text {
                return Err(format!(
                    "{table}: {action:?} belongs in {}",
                    if text { "keybinds" } else { "text_keybinds" }
                ));
            }
            for value in keys {
                let binding = Keybind::parse(&value).map_err(|error| {
                    format!("{table}: invalid shortcut {value:?} for {action:?}: {error}")
                })?;
                // Shift extends text selections for every configured navigation binding.
                let selection = (text && action.movement().is_some() && !binding.modifiers.shift)
                    .then_some(Keybind {
                        modifiers: Modifiers {
                            shift: true,
                            ..binding.modifiers
                        },
                        ..binding
                    });
                for (binding, automatic_selection) in std::iter::once((binding, false))
                    .chain(selection.map(|binding| (binding, true)))
                {
                    if let Some(previous) = bindings
                        .iter()
                        .find(|entry| entry.key == binding && entry.action != action)
                    {
                        let previous = previous.action;
                        return Err(format!(
                            "{table}: {action:?} and {previous:?} both use {binding}. Remove the shortcut from one action or set that action to []"
                        ));
                    }
                    if bindings.iter().any(|entry| {
                        entry.key == binding && entry.automatic_selection == automatic_selection
                    }) {
                        continue;
                    }
                    bindings.push(ResolvedKeybind {
                        key: binding,
                        action,
                        automatic_selection,
                    });
                }
            }
        }
        Ok(Self(bindings))
    }

    pub(crate) fn lookup(
        &self,
        key: u32,
        shift_free_key: Option<u32>,
        modifiers: Modifiers,
        logo: bool,
        gestures_only: bool,
    ) -> Option<BindingAction> {
        // Ignore letter case from Caps Lock while matching Shift explicitly.
        let normalize = |key: u32| {
            let name = xkb::keysym_get_name(key.into());
            xkb::keysym_from_name(&name, xkb::KEYSYM_CASE_INSENSITIVE).raw()
        };
        let key = normalize(key);
        let chord = Keybind {
            key: Input::Keyboard(key),
            modifiers,
            logo,
        };
        let explicit = |chord: Keybind| {
            self.0.iter().find(move |entry| {
                !entry.automatic_selection
                    && entry.key == chord
                    && (!gestures_only || entry.action.pointer_gesture())
            })
        };
        // Prefer explicit symbols, then match the key without Shift.
        if let Some(entry) = explicit(chord) {
            return Some(entry.action);
        }
        let shift_free_key = shift_free_key.map(normalize);
        shift_free_key
            .and_then(|key| {
                let selection = Keybind {
                    key: Input::Keyboard(key),
                    ..chord
                };
                self.0.iter().find(|entry| {
                    entry.key == selection && (!gestures_only || entry.action.pointer_gesture())
                })
            })
            .or_else(|| {
                // Shift produced the symbol, as with plus on US layouts.
                if shift_free_key? == key {
                    return None;
                }
                explicit(Keybind {
                    modifiers: Modifiers {
                        shift: false,
                        ..modifiers
                    },
                    ..chord
                })
            })
            .or_else(|| {
                let plain = Keybind {
                    key: Input::Keyboard(shift_free_key.unwrap_or(key)),
                    modifiers: Modifiers::default(),
                    logo: false,
                };
                self.0
                    .iter()
                    .find(|entry| entry.key == plain && entry.action.pointer_gesture())
            })
            .map(|entry| entry.action)
    }

    pub(crate) fn mouse_binding(
        &self,
        button: MouseButton,
        modifiers: Modifiers,
        logo: bool,
        gestures_only: bool,
    ) -> Option<BindingAction> {
        let chord = Keybind {
            key: Input::Mouse(button),
            modifiers,
            logo,
        };
        self.0
            .iter()
            .find(|entry| entry.key == chord && (!gestures_only || entry.action.pointer_gesture()))
            .or_else(|| {
                // Drawing modifiers still work with unmodified gesture bindings.
                let plain = Keybind {
                    modifiers: Modifiers::default(),
                    logo: false,
                    ..chord
                };
                self.0
                    .iter()
                    .find(|entry| entry.action.pointer_gesture() && entry.key == plain)
            })
            .map(|entry| entry.action)
    }
}
