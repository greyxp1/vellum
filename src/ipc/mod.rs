mod client;

pub(crate) use client::{query, send_command};

use std::borrow::Cow;
use std::fs::File;
use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;

use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SocketAddrAny};

use crate::cli::{Command, VisibilityCommand, WhiteboardCommand};

fn control_socket_path() -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStrExt;
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .ok_or("XDG_RUNTIME_DIR is not set")?;
    let display = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into());
    let socket = runtime_dir.join(display);
    let socket = socket.canonicalize().unwrap_or(socket);
    // FNV-1a keeps the name short and stable across installed and development builds.
    let hash = socket
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    Ok(runtime_dir.join(format!("vellum-{hash:016x}.sock")))
}

pub(crate) struct ControlSocket {
    pub socket: UnixDatagram,
    path: PathBuf,
    _lock: File,
}

impl ControlSocket {
    pub(crate) fn bind() -> Result<Self, String> {
        let path = control_socket_path()?;
        // Holding the lock makes any existing socket a leftover from an unclean exit.
        let lock = File::create(path.with_extension("lock"))
            .map_err(|error| format!("could not create control socket lock: {error}"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| "another instance is already running")?;
        let _ = std::fs::remove_file(&path);
        let socket = UnixDatagram::bind(&path)
            .map_err(|error| format!("could not bind control socket: {error}"))?;
        let control = Self {
            socket,
            path,
            _lock: lock,
        };
        rustix::net::sockopt::set_socket_passcred(&control.socket, true)
            .map_err(|error| format!("could not enable control socket credentials: {error}"))?;
        control
            .socket
            .set_nonblocking(true)
            .map_err(|error| format!("could not configure control socket: {error}"))?;
        Ok(control)
    }

    pub(crate) fn recv(
        &self,
        message: &mut [u8],
    ) -> std::io::Result<Option<(usize, Option<SocketAddrAny>)>> {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmCredentials(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let received = rustix::net::recvmsg(
            &self.socket,
            &mut [IoSliceMut::new(message)],
            &mut ancillary,
            RecvFlags::CMSG_CLOEXEC,
        )?;
        let authorized = ancillary.drain().any(|message| {
            matches!(message, RecvAncillaryMessage::ScmCredentials(credentials)
                if credentials.uid == rustix::process::geteuid())
        });
        if !authorized || received.flags.contains(rustix::net::ReturnFlags::CTRUNC) {
            return Ok(None);
        }
        Ok(Some((received.bytes, received.address)))
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Command {
    fn serialize(&self) -> Cow<'static, str> {
        match self {
            Self::Toggle => "toggle".into(),
            Self::Activate => "activate".into(),
            Self::Deactivate => "deactivate".into(),
            Self::Clear => "clear".into(),
            Self::ClearAndDeactivate => "clear_and_deactivate".into(),
            Self::IsActive => "is_active".into(),
            Self::SetColor { color } => format!("set_color={color}").into(),
            Self::IsTextEditing => "is_text_editing".into(),
            Self::IsWhiteboard => "is_whiteboard".into(),
            Self::Exit => "exit".into(),
            Self::Annotations { command } => match command {
                VisibilityCommand::Show => "annotations_show".into(),
                VisibilityCommand::Hide => "annotations_hide".into(),
                VisibilityCommand::Toggle => "annotations_toggle".into(),
            },
            Self::Whiteboard { command } => match command {
                WhiteboardCommand::Show => "whiteboard_show".into(),
                WhiteboardCommand::Hide => "whiteboard_hide".into(),
                WhiteboardCommand::Toggle => "whiteboard_toggle".into(),
                WhiteboardCommand::Clear => "whiteboard_clear".into(),
                WhiteboardCommand::SetColor { color } => format!("whiteboard_color={color}").into(),
            },
        }
    }

    pub(crate) fn deserialize(message: &[u8]) -> Result<Self, &'static str> {
        match message {
            b"toggle" => Ok(Self::Toggle),
            b"activate" => Ok(Self::Activate),
            b"deactivate" => Ok(Self::Deactivate),
            b"clear" => Ok(Self::Clear),
            b"clear_and_deactivate" => Ok(Self::ClearAndDeactivate),
            b"is_active" => Ok(Self::IsActive),
            b"is_text_editing" => Ok(Self::IsTextEditing),
            b"is_whiteboard" => Ok(Self::IsWhiteboard),
            b"exit" => Ok(Self::Exit),
            b"annotations_show" => Ok(Self::Annotations {
                command: VisibilityCommand::Show,
            }),
            b"annotations_hide" => Ok(Self::Annotations {
                command: VisibilityCommand::Hide,
            }),
            b"annotations_toggle" => Ok(Self::Annotations {
                command: VisibilityCommand::Toggle,
            }),
            b"whiteboard_show" => Ok(Self::Whiteboard {
                command: WhiteboardCommand::Show,
            }),
            b"whiteboard_hide" => Ok(Self::Whiteboard {
                command: WhiteboardCommand::Hide,
            }),
            b"whiteboard_toggle" => Ok(Self::Whiteboard {
                command: WhiteboardCommand::Toggle,
            }),
            b"whiteboard_clear" => Ok(Self::Whiteboard {
                command: WhiteboardCommand::Clear,
            }),
            _ if let Some(color) = message.strip_prefix(b"whiteboard_color=") => {
                let color = crate::cli::opaque_color(color_str(color)?)
                    .map_err(|_| "invalid or transparent whiteboard color")?;
                Ok(Self::Whiteboard {
                    command: WhiteboardCommand::SetColor { color },
                })
            }
            _ if let Some(color) = message.strip_prefix(b"set_color=") => Ok(Self::SetColor {
                color: crate::cli::visible_color(color_str(color)?)
                    .map_err(|_| "invalid or transparent color")?,
            }),
            _ => Err("invalid command"),
        }
    }
}

fn color_str(color: &[u8]) -> Result<&str, &'static str> {
    std::str::from_utf8(color).map_err(|_| "expected color to be valid utf8")
}
