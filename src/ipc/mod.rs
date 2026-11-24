mod client;

pub(crate) use client::send;

use std::fs::File;
use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::PathBuf;

use clap::ValueEnum;
use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SocketAddrAny};

use crate::cli::Visibility::{Hide, Show, Toggle};
use crate::cli::WhiteboardCommand::{self as Board, Visibility as BoardView};
use crate::cli::{self, Command, StatusField, ToolSetting};

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

/// Datagrams of the commands without a payload.
const FIXED_COMMANDS: [(&str, Command); 16] = [
    ("toggle", Command::Toggle),
    ("activate", Command::Activate),
    ("deactivate", Command::Deactivate),
    ("clear", Command::Clear { deactivate: false }),
    ("clear_and_deactivate", Command::Clear { deactivate: true }),
    ("exit", Command::Exit),
    ("annotations_show", Command::Annotations(Show)),
    ("annotations_hide", Command::Annotations(Hide)),
    ("annotations_toggle", Command::Annotations(Toggle)),
    ("presentation_show", Command::Presentation(Show)),
    ("presentation_hide", Command::Presentation(Hide)),
    ("presentation_toggle", Command::Presentation(Toggle)),
    ("whiteboard_show", Command::Whiteboard(BoardView(Show))),
    ("whiteboard_hide", Command::Whiteboard(BoardView(Hide))),
    ("whiteboard_toggle", Command::Whiteboard(BoardView(Toggle))),
    ("whiteboard_clear", Command::Whiteboard(Board::Clear)),
];

/// Splits a `name[@tool][=payload]` request.
fn split_request(request: &str) -> (&str, Option<&str>, Option<&str>) {
    let (target, payload) = request
        .split_once('=')
        .map_or((request, None), |(target, payload)| (target, Some(payload)));
    let (name, tool) = target
        .split_once('@')
        .map_or((target, None), |(name, tool)| (name, Some(tool)));
    (name, tool, payload)
}

fn parse_tool(tool: Option<&str>, error: &'static str) -> Result<Option<String>, &'static str> {
    tool.map(|tool| cli::tool_name(tool).map_err(|_| error))
        .transpose()
}

impl Command {
    fn serialize(&self) -> String {
        let target = |tool: &Option<String>| {
            tool.as_ref()
                .map(|tool| format!("@{tool}"))
                .unwrap_or_default()
        };
        match self {
            Self::Use { tool } => format!("use={tool}"),
            Self::Set { tool, setting } => {
                let (property, value) = match setting {
                    ToolSetting::Size { value } => ("size", value.to_string()),
                    ToolSetting::Opacity { value } => ("opacity", value.to_string()),
                    ToolSetting::Roundness { value } => ("roundness", value.to_string()),
                    ToolSetting::Filled { value } => ("filled", value.to_string()),
                    ToolSetting::Color { value } => ("color", value.to_string()),
                };
                format!("set{}={property}={value}", target(tool))
            }
            Self::Status {
                field, tool, json, ..
            } => {
                let name = if *json { "status_json" } else { "status" };
                let field = field
                    .map(|field| format!("={}", field.to_possible_value().unwrap().get_name()))
                    .unwrap_or_default();
                format!("{name}{}{field}", target(tool))
            }
            Self::Whiteboard(Board::SetColor { color }) => format!("whiteboard_color={color}"),
            fixed @ (Self::Toggle
            | Self::Activate
            | Self::Deactivate
            | Self::Clear { deactivate: false }
            | Self::Clear { deactivate: true }
            | Self::Exit
            | Self::Annotations(Show)
            | Self::Annotations(Hide)
            | Self::Annotations(Toggle)
            | Self::Presentation(Show)
            | Self::Presentation(Hide)
            | Self::Presentation(Toggle)
            | Self::Whiteboard(BoardView(Show))
            | Self::Whiteboard(BoardView(Hide))
            | Self::Whiteboard(BoardView(Toggle))
            | Self::Whiteboard(Board::Clear)) => {
                let (name, _) = FIXED_COMMANDS
                    .iter()
                    .find(|(_, command)| command == fixed)
                    .expect("commands without a payload have a fixed datagram");
                name.to_string()
            }
        }
    }

    pub(crate) fn deserialize(message: &[u8]) -> Result<Self, &'static str> {
        let text = |error| std::str::from_utf8(message).map_err(|_| error);
        match message {
            _ if let Some(tool) = message.strip_prefix(b"use=") => {
                let tool = std::str::from_utf8(tool).map_err(|_| "invalid tool")?;
                Ok(Self::Use {
                    tool: cli::tool_name(tool).map_err(|_| "invalid tool")?,
                })
            }
            _ if message.starts_with(b"set=") || message.starts_with(b"set@") => {
                let (_, tool, setting) = split_request(text("invalid setter request")?);
                let setting = setting.ok_or("missing setting")?;
                let tool = parse_tool(tool, "invalid tool")?;
                let (property, value) = setting.split_once('=').ok_or("missing value")?;
                use crate::cli::{opacity, positive_size, roundness};
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
                        value: cli::visible_color(value).map_err(|_| "invalid color")?,
                    },
                    _ => return Err("invalid tool property"),
                };
                Ok(Self::Set { tool, setting })
            }
            _ if message.starts_with(b"status") => {
                let (name, tool, field) = split_request(text("invalid status request")?);
                let json = match name {
                    "status" => false,
                    "status_json" => true,
                    _ => return Err("invalid status request"),
                };
                let tool = parse_tool(tool, "invalid status tool")?;
                let field = field
                    .map(|field| StatusField::from_str(field, false))
                    .transpose()
                    .map_err(|_| "invalid status field")?;
                if tool.is_some() && field.is_some_and(|field| !field.is_tool_field()) {
                    return Err("tool target requires a tool property");
                }
                Ok(Self::Status {
                    field,
                    tool,
                    json,
                    check: false,
                })
            }
            _ if let Some(color) = message.strip_prefix(b"whiteboard_color=") => {
                let color =
                    std::str::from_utf8(color).map_err(|_| "expected color to be valid utf8")?;
                let color = cli::opaque_color(color)
                    .map_err(|_| "invalid or transparent whiteboard color")?;
                Ok(Self::Whiteboard(Board::SetColor { color }))
            }
            _ => FIXED_COMMANDS
                .iter()
                .find(|(name, _)| name.as_bytes() == message)
                .map(|(_, command)| command.clone())
                .ok_or("invalid command"),
        }
    }
}
