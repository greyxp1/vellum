use std::borrow::Cow;

use color::DynamicColor;

use crate::cli::Command;

pub const CONTROL_SOCKET: &str = "vellum.sock";

impl Command {
    pub fn serialize(&self) -> Cow<'static, str> {
        match self {
            Self::Toggle => "toggle".into(),
            Self::Activate => "activate".into(),
            Self::Deactivate => "deactivate".into(),
            Self::Clear => "clear".into(),
            Self::ClearAndDeactivate => "clear_and_deactivate".into(),
            Self::IsActive => "is_active".into(),
            Self::SetColor { color } => format!("set_color={color}").into(),
            Self::IsTextEditing => "is_text_editing".into(),
            Self::Exit => "exit".into(),
        }
    }

    pub fn deserialize(message: &[u8]) -> Result<Self, &'static str> {
        match message {
            b"toggle" => Ok(Self::Toggle),
            b"activate" => Ok(Self::Activate),
            b"deactivate" => Ok(Self::Deactivate),
            b"clear" => Ok(Self::Clear),
            b"clear_and_deactivate" => Ok(Self::ClearAndDeactivate),
            b"is_active" => Ok(Self::IsActive),
            b"is_text_editing" => Ok(Self::IsTextEditing),
            b"exit" => Ok(Self::Exit),
            _ if let Some(color) = message.strip_prefix(b"set_color=") => Ok(Self::SetColor {
                color: color_from_utf8(color)?,
            }),
            _ => Err("invalid command"),
        }
    }
}

fn color_from_utf8(msg: &[u8]) -> Result<DynamicColor, &'static str> {
    std::str::from_utf8(msg)
        .map_err(|_| "expected color to be valid utf8")?
        .parse()
        .map_err(|_| "invalid color")
}
