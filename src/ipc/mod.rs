mod client;

pub(crate) use client::{query, send_command};

use std::borrow::Cow;
use std::fs::File;
use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::PathBuf;

use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SocketAddrAny};

use crate::cli::{Command, PresentationCommand, ToolSetting, VisibilityCommand, WhiteboardCommand};

fn control_socket_addresses() -> Result<[SocketAddr; 2], String> {
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
    let path = runtime_dir.join(format!("vellum-{hash:016x}.sock"));
    let legacy_name = format!("vellum-{}-{hash:016x}", rustix::process::geteuid().as_raw());
    let address_error = |error| format!("invalid control socket address: {error}");
    Ok([
        SocketAddr::from_pathname(path).map_err(address_error)?,
        SocketAddr::from_abstract_name(legacy_name).map_err(address_error)?,
    ])
}

pub(crate) struct ControlSocket {
    pub sockets: [UnixDatagram; 2],
    path: PathBuf,
    _lock: File,
}

impl ControlSocket {
    pub(crate) fn bind() -> Result<Self, String> {
        let addresses = control_socket_addresses()?;
        let path = addresses[0].as_pathname().unwrap().to_owned();
        // Holding the lock makes any existing socket a leftover from an unclean exit.
        let lock = File::create(path.with_extension("lock"))
            .map_err(|error| format!("could not create control socket lock: {error}"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| "another instance is already running")?;
        let _ = std::fs::remove_file(&path);
        // Older installed clients use the abstract address. Bind it first so an
        // older daemon also prevents a second instance from starting.
        let legacy = UnixDatagram::bind_addr(&addresses[1])
            .map_err(|error| format!("could not bind control socket: {error}"))?;
        let socket = UnixDatagram::bind_addr(&addresses[0])
            .map_err(|error| format!("could not bind control socket: {error}"))?;
        let control = Self {
            sockets: [socket, legacy],
            path,
            _lock: lock,
        };
        for socket in &control.sockets {
            rustix::net::sockopt::set_socket_passcred(socket, true)
                .map_err(|error| format!("could not enable control socket credentials: {error}"))?;
            socket
                .set_nonblocking(true)
                .map_err(|error| format!("could not configure control socket: {error}"))?;
        }
        Ok(control)
    }

    pub(crate) fn recv(
        socket: &UnixDatagram,
        message: &mut [u8],
    ) -> std::io::Result<Option<(usize, Option<SocketAddrAny>)>> {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmCredentials(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let received = rustix::net::recvmsg(
            socket,
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
            Self::Use { tool } => format!("use={}", tool.name()).into(),
            Self::Set { tool, setting } => {
                let (property, value) = match setting {
                    ToolSetting::Size { value } => ("size", value.to_string()),
                    ToolSetting::Opacity { value } => ("opacity", value.to_string()),
                    ToolSetting::Roundness { value } => ("roundness", value.to_string()),
                    ToolSetting::Filled { value } => ("filled", value.to_string()),
                    ToolSetting::Color { value } => ("color", value.to_string()),
                };
                let target = tool.map_or(String::new(), |tool| format!("@{}", tool.name()));
                format!("set{target}={property}={value}").into()
            }
            Self::Status {
                field, tool, json, ..
            } => {
                let name = if *json { "status_json" } else { "status" };
                let name: Cow<'_, str> = match tool {
                    Some(tool) => format!("{name}@{}", tool.name()).into(),
                    None => Cow::Borrowed(name),
                };
                match field {
                    None => name,
                    Some(field) => format!(
                        "{name}={}",
                        clap::ValueEnum::to_possible_value(field)
                            .unwrap()
                            .get_name()
                    )
                    .into(),
                }
            }
            Self::Toggle => "toggle".into(),
            Self::Activate => "activate".into(),
            Self::Deactivate => "deactivate".into(),
            Self::Clear { deactivate: false } => "clear".into(),
            Self::Clear { deactivate: true } => "clear_and_deactivate".into(),
            Self::Exit => "exit".into(),
            Self::Annotations { command } => match command {
                VisibilityCommand::Show => "annotations_show".into(),
                VisibilityCommand::Hide => "annotations_hide".into(),
                VisibilityCommand::Toggle => "annotations_toggle".into(),
            },
            Self::Presentation { command } => match command {
                PresentationCommand::Show => "presentation_show".into(),
                PresentationCommand::Hide => "presentation_hide".into(),
                PresentationCommand::Toggle => "presentation_toggle".into(),
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
            _ if let Some(tool) = message.strip_prefix(b"use=") => {
                let tool = std::str::from_utf8(tool).map_err(|_| "invalid tool")?;
                Ok(Self::Use {
                    tool: clap::ValueEnum::from_str(tool, false).map_err(|_| "invalid tool")?,
                })
            }
            _ if message.starts_with(b"set=") || message.starts_with(b"set@") => {
                let message = std::str::from_utf8(message).map_err(|_| "invalid setter request")?;
                let (target, setting) = message.split_once('=').ok_or("missing setting")?;
                let tool = target
                    .strip_prefix("set@")
                    .map(|tool| clap::ValueEnum::from_str(tool, false).map_err(|_| "invalid tool"))
                    .transpose()?;
                let (property, value) = setting.split_once('=').ok_or("missing value")?;
                use crate::cli::{opacity, positive_size, roundness, visible_color};
                let setting = match property {
                    "size" => ToolSetting::Size {
                        value: positive_size(value).map_err(|_| "invalid size")?,
                    },
                    "opacity" => ToolSetting::Opacity {
                        value: opacity(value).map_err(|_| "invalid opacity")?,
                    },
                    "roundness" => ToolSetting::Roundness {
                        value: roundness(value).map_err(|_| "invalid roundness")?,
                    },
                    "filled" => ToolSetting::Filled {
                        value: value.parse().map_err(|_| "invalid fill")?,
                    },
                    "color" => ToolSetting::Color {
                        value: visible_color(value).map_err(|_| "invalid color")?,
                    },
                    _ => return Err("invalid tool property"),
                };
                Ok(Self::Set { tool, setting })
            }
            _ if message.starts_with(b"status") => {
                let message = std::str::from_utf8(message).map_err(|_| "invalid status request")?;
                let (target, field) = message
                    .split_once('=')
                    .map_or((message, None), |(target, field)| (target, Some(field)));
                let (name, tool) = target
                    .split_once('@')
                    .map_or((target, None), |(name, tool)| (name, Some(tool)));
                let json = match name {
                    "status" => false,
                    "status_json" => true,
                    _ => return Err("invalid status request"),
                };
                let tool = tool
                    .map(|tool| {
                        clap::ValueEnum::from_str(tool, false).map_err(|_| "invalid status tool")
                    })
                    .transpose()?;
                let field = field
                    .map(|field| {
                        clap::ValueEnum::from_str(field, false).map_err(|_| "invalid status field")
                    })
                    .transpose()?;
                if tool.is_some()
                    && field.is_some_and(|field: crate::cli::StatusField| !field.is_tool_field())
                {
                    return Err("tool target requires a tool property");
                }
                Ok(Self::Status {
                    field,
                    tool,
                    json,
                    check: false,
                })
            }
            b"toggle" => Ok(Self::Toggle),
            b"activate" => Ok(Self::Activate),
            b"deactivate" => Ok(Self::Deactivate),
            b"clear" => Ok(Self::Clear { deactivate: false }),
            b"clear_and_deactivate" => Ok(Self::Clear { deactivate: true }),
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
            b"presentation_show" => Ok(Self::Presentation {
                command: PresentationCommand::Show,
            }),
            b"presentation_hide" => Ok(Self::Presentation {
                command: PresentationCommand::Hide,
            }),
            b"presentation_toggle" => Ok(Self::Presentation {
                command: PresentationCommand::Toggle,
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
            _ => Err("invalid command"),
        }
    }
}

fn color_str(color: &[u8]) -> Result<&str, &'static str> {
    std::str::from_utf8(color).map_err(|_| "expected color to be valid utf8")
}
