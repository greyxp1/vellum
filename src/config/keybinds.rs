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
    Copy,
    Cut,
    Paste,
    SelectAll,
    Delete,
    Cancel,
    Clear,
    ToggleFill,
    ToggleEraser,
    ToggleWhiteboard,
    TogglePresentation,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gesture {
    Draw,
    Picker,
    Eraser,
}

impl BindingAction {
    pub(crate) fn gesture(self) -> Option<Gesture> {
        match self {
            Self::Draw => Some(Gesture::Draw),
            Self::OpenPicker => Some(Gesture::Picker),
            Self::Erase => Some(Gesture::Eraser),
            _ => None,
        }
    }

    pub(crate) fn repeatable(self) -> bool {
        !matches!(
            self,
            Self::ToggleFill
                | Self::ToggleEraser
                | Self::ToggleWhiteboard
                | Self::TogglePresentation
                | Self::ToggleFreeze
                | Self::Copy
                | Self::Cut
                | Self::Paste
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
            Self::Copy => Action::Copy,
            Self::Cut => Action::Cut,
            Self::Paste => Action::Paste,
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
pub(crate) enum Input {
    Keyboard(u32),
    Mouse(MouseButton),
}

impl Input {
    // Ignore letter case from Caps Lock while matching Shift explicitly.
    fn normalized(self) -> Self {
        match self {
            Self::Keyboard(key) => {
                let name = xkb::keysym_get_name(key.into());
                Self::Keyboard(xkb::keysym_from_name(&name, xkb::KEYSYM_CASE_INSENSITIVE).raw())
            }
            mouse => mouse,
        }
    }
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
                (Undo, &["Ctrl+z"]),
                (Redo, &["Ctrl+Shift+z", "Ctrl+y"]),
                (Copy, &["Ctrl+c"]),
                (Cut, &["Ctrl+x"]),
                (Paste, &["Ctrl+v"]),
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
                (Clear, &["Ctrl+Delete"]),
                (SelectAll, &["Ctrl+a"]),
                (Undo, &["Ctrl+z", "MouseBack"]),
                (Redo, &["Ctrl+Shift+z", "Ctrl+y", "MouseForward"]),
                (ToggleFill, &["f"]),
                (ToggleWhiteboard, &["w"]),
                (TogglePresentation, &["p"]),
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
                || matches!(
                    action,
                    CommitText | Newline | Backspace | BackspaceWord | Copy | Cut | Paste
                );
            let shared_action = matches!(action, Cancel | Delete | SelectAll | Undo | Redo);
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
        input: Input,
        shift_free: Option<Input>,
        modifiers: Modifiers,
        logo: bool,
        gestures_only: bool,
    ) -> Option<BindingAction> {
        let (input, shift_free) = (input.normalized(), shift_free.map(Input::normalized));
        let chord = Keybind {
            key: input,
            modifiers,
            logo,
        };
        let unshifted = Keybind {
            modifiers: Modifiers {
                shift: false,
                ..modifiers
            },
            ..chord
        };
        // Drawing modifiers still work with unmodified gesture bindings.
        let plain = Keybind {
            key: shift_free.unwrap_or(input),
            modifiers: Modifiers::default(),
            logo: false,
        };
        // Prefer explicit symbols, then match the key without Shift. Candidates also
        // choose whether generated selections and non-gesture actions match.
        [
            Some((chord, false, gestures_only)),
            shift_free.map(|key| (Keybind { key, ..chord }, true, gestures_only)),
            // Shift produced the symbol, as with plus on US layouts.
            shift_free
                .filter(|&key| key != input)
                .map(|_| (unshifted, false, gestures_only)),
            Some((plain, true, true)),
        ]
        .into_iter()
        .flatten()
        .find_map(|(key, automatic, gestures)| {
            self.0.iter().find(|entry| {
                entry.key == key
                    && (automatic || !entry.automatic_selection)
                    && (!gestures || entry.action.gesture().is_some())
            })
        })
        .map(|entry| entry.action)
    }
}
