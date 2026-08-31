use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::SocketAddr;
use std::os::unix::net::UnixDatagram;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::backend::WaylandError;

mod cli;
mod config;
mod protocol;
mod render;
mod state;

use cli::{Cli, Command};
use config::Settings;
use protocol::CONTROL_SOCKET;

const MAX_SOCKET_MESSAGE: usize = 4096;
pub(crate) type Rgb = [f32; 3];

fn main() -> ExitCode {
    match run() {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("vellum: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let arguments = Cli::parse();
    if let Some(subcommand) = &arguments.command {
        let truthy_value = match subcommand {
            Command::IsActive => query_active()?,
            Command::IsTextEditing => query_text_editing()?,
            _ => {
                send_command(subcommand)?;
                return Ok(ExitCode::SUCCESS);
            }
        };
        println!("{truthy_value}");
        return Ok(if truthy_value {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }
    let settings = Settings::load(arguments)?;
    run_overlay(settings)?;
    Ok(ExitCode::SUCCESS)
}

fn send_command(command: &Command) -> Result<(), String> {
    let socket_addr =
        SocketAddr::from_abstract_name(CONTROL_SOCKET).map_err(|error| error.to_string())?;
    let socket = UnixDatagram::unbound().map_err(|error| error.to_string())?;
    socket
        .connect_addr(&socket_addr)
        .map_err(|error| format!("could not connect to the overlay: {error}"))?;
    socket
        .send(command.serialize())
        .map_err(|error| format!("could not send command: {error}"))?;
    Ok(())
}

fn query_active() -> Result<bool, String> {
    query(Command::IsActive.serialize())
}

fn query_text_editing() -> Result<bool, String> {
    query(Command::IsTextEditing.serialize())
}

fn query(request: &[u8]) -> Result<bool, String> {
    let socket_addr =
        SocketAddr::from_abstract_name(CONTROL_SOCKET).map_err(|error| error.to_string())?;
    let reply_name = format!(
        "vellum-query-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos()
    );
    let reply_addr =
        SocketAddr::from_abstract_name(reply_name).map_err(|error| error.to_string())?;
    let socket = UnixDatagram::bind_addr(&reply_addr).map_err(|error| error.to_string())?;
    if let Err(error) = socket.connect_addr(&socket_addr) {
        if error.kind() == std::io::ErrorKind::ConnectionRefused {
            return Ok(false);
        }
        return Err(format!("could not connect to the overlay: {error}"));
    }
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|error| format!("could not configure control socket: {error}"))?;
    if let Err(error) = socket.send(request) {
        if error.kind() == std::io::ErrorKind::ConnectionRefused {
            return Ok(false);
        }
        return Err(format!("could not send command: {error}"));
    }

    let mut response = [0; 5];
    let size = match socket.recv(&mut response) {
        Ok(size) => size,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Ok(false);
        }
        Err(error) => return Err(format!("could not receive overlay status: {error}")),
    };
    let active = match &response[..size] {
        b"true" => true,
        b"false" => false,
        _ => return Err("invalid overlay status".into()),
    };
    Ok(active)
}

fn run_overlay(settings: Settings) -> Result<(), String> {
    let socket_addr = SocketAddr::from_abstract_name(CONTROL_SOCKET)
        .map_err(|error| format!("invalid control socket name: {error}"))?;
    let socket = UnixDatagram::bind_addr(&socket_addr)
        .map_err(|error| format!("could not bind control socket: {error}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure control socket: {error}"))?;

    let (mut state, mut event_queue) = state::State::setup_wayland(settings)?;
    state.deactivate();

    loop {
        event_queue
            .dispatch_pending(&mut state)
            .map_err(|error| format!("Wayland dispatch failed: {error}"))?;
        let flush_blocked = match event_queue.flush() {
            Ok(()) => false,
            Err(WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(error) => return Err(format!("Wayland flush failed: {error}")),
        };

        let Some(read_guard) = event_queue.prepare_read() else {
            continue;
        };
        let timeout = state.next_wakeup().map(|deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            Timespec {
                tv_sec: remaining.as_secs() as _,
                tv_nsec: remaining.subsec_nanos() as _,
            }
        });
        let (wayland_ready, socket_ready) = {
            let mut fds = [
                PollFd::new(
                    &event_queue,
                    PollFlags::IN
                        | if flush_blocked {
                            PollFlags::OUT
                        } else {
                            PollFlags::empty()
                        },
                ),
                PollFd::new(&socket, PollFlags::IN),
            ];
            if let Err(error) = poll(&mut fds, timeout.as_ref()) {
                if error == rustix::io::Errno::INTR {
                    continue;
                }
                return Err(format!("event polling failed: {error}"));
            }
            (
                fds[0].revents().contains(PollFlags::IN),
                fds[1].revents().contains(PollFlags::IN),
            )
        };
        if wayland_ready {
            read_guard
                .read()
                .map_err(|error| format!("Wayland read failed: {error}"))?;
        } else {
            drop(read_guard);
        }

        if socket_ready {
            let mut message = [0; MAX_SOCKET_MESSAGE + 1];
            loop {
                let (size, sender) = match socket.recv_from(&mut message) {
                    Ok(message) => message,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(format!("socket read failed: {error}")),
                };
                if size > MAX_SOCKET_MESSAGE {
                    eprintln!("vellum: socket message exceeded {MAX_SOCKET_MESSAGE} bytes");
                    continue;
                }
                let command = match Command::deserialize(&message[..size]) {
                    Ok(command) => command,
                    Err(error) => {
                        eprintln!("{error}");
                        continue;
                    }
                };
                match command {
                    Command::Toggle => state.toggle_input(),
                    Command::Activate => state.set_input_active(true),
                    Command::Deactivate => state.set_input_active(false),
                    Command::Clear => state.clear(),
                    Command::ClearAndDeactivate => {
                        state.clear();
                        state.set_input_active(false);
                    }
                    Command::IsActive => {
                        let response: &[u8] = if state.is_active() { b"true" } else { b"false" };
                        if let Err(error) = socket.send_to_addr(response, &sender) {
                            eprintln!("vellum: could not send status: {error}");
                        }
                    }
                    Command::IsTextEditing => {
                        let response: &[u8] = if state.is_text_editing() {
                            b"true"
                        } else {
                            b"false"
                        };
                        if let Err(error) = socket.send_to_addr(response, &sender) {
                            eprintln!("vellum: could not send status: {error}");
                        }
                    }
                    Command::Exit => return Ok(()),
                }
            }
        }
        state.handle_timeouts(Instant::now());
    }
}
