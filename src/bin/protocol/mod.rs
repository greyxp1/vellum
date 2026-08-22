use crate::cli::Command;

pub const CONTROL_SOCKET: &str = "vellum.sock";

impl Command {
    pub fn serialize(&self) -> &'static [u8] {
        match self {
            Self::Toggle => b"toggle",
            Self::Activate => b"activate",
            Self::Deactivate => b"deactivate",
            Self::Clear => b"clear",
            Self::ClearAndDeactivate => b"clear_and_deactivate",
            Self::IsActive => b"is_active",
            Self::Exit => b"exit",
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
            b"exit" => Ok(Self::Exit),
            _ => Err("invalid command"),
        }
    }
}
