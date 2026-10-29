use std::time::Instant;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::backend::WaylandError;

use crate::cli::Command;
use crate::config::{Config, Settings};
use crate::ipc::ControlSocket;
use crate::{state, text};

const MAX_SOCKET_MESSAGE: usize = 4096;

pub(crate) fn run(mut settings: Settings, mut config: Config) -> Result<(), String> {
    text::set_text_font(settings.text_font.clone());
    let control = ControlSocket::bind()?;
    let socket = &control.socket;

    let (mut state, mut event_queue) = state::State::setup_wayland(&settings)?;
    let mut pending_settings = None;

    loop {
        event_queue
            .dispatch_pending(&mut state)
            .map_err(|error| format!("Wayland dispatch failed: {error}"))?;
        if let Some(result) = config.reload(Instant::now()) {
            match result {
                Ok(settings) => pending_settings = Some(settings),
                Err(error) => {
                    log::warn!("config reload failed, keeping previous settings: {error}")
                }
            }
        }
        if state.can_reload()
            && let Some(next) = pending_settings.take()
        {
            state.reload(&next, &settings);
            settings = next;
            log::info!("configuration reloaded");
        }
        // Coalesce input, cursor and drawing changes before presenting.
        state.render_pending();
        if let Some(error) = state.fatal_error.take() {
            return Err(error);
        }
        state.sync_text_input();
        let flush_blocked = match event_queue.flush() {
            Ok(()) => false,
            Err(WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(error) => return Err(format!("Wayland flush failed: {error}")),
        };

        let Some(read_guard) = event_queue.prepare_read() else {
            continue;
        };
        let timeout = state
            .next_wakeup()
            .into_iter()
            .chain(config.next_wakeup())
            .min()
            .map(|deadline| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                Timespec {
                    tv_sec: remaining.as_secs() as _,
                    tv_nsec: remaining.subsec_nanos() as _,
                }
            });
        let (wayland_ready, socket_ready, config_ready) = {
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
                PollFd::new(socket, PollFlags::IN),
                config.fd().map_or_else(
                    || PollFd::new(socket, PollFlags::empty()),
                    |fd| PollFd::new(fd, PollFlags::IN),
                ),
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
                fds[2].revents().contains(PollFlags::IN),
            )
        };
        if wayland_ready {
            read_guard
                .read()
                .map_err(|error| format!("Wayland read failed: {error}"))?;
        } else {
            drop(read_guard);
        }

        if config_ready {
            config.changed();
        }

        if socket_ready {
            let mut message = [0; MAX_SOCKET_MESSAGE + 1];
            loop {
                let (size, sender) = match control.recv(&mut message) {
                    Ok(Some(message)) => message,
                    Ok(None) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(format!("socket read failed: {error}")),
                };
                if size > MAX_SOCKET_MESSAGE {
                    log::warn!("socket message exceeded {MAX_SOCKET_MESSAGE} bytes");
                    continue;
                }
                let command = match Command::deserialize(&message[..size]) {
                    Ok(command) => command,
                    Err(error) => {
                        log::warn!("ignored socket message: {error}");
                        continue;
                    }
                };
                let reply = |active: bool| {
                    let response: &[u8] = if active { b"true" } else { b"false" };
                    if let Some(sender) = &sender
                        && let Err(error) = rustix::net::sendto(
                            socket,
                            response,
                            rustix::net::SendFlags::empty(),
                            sender,
                        )
                    {
                        log::warn!("could not send status: {error}");
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
                    Command::SetColor { color } => state.set_current_color(color),
                    Command::Whiteboard { command } => state.whiteboard(command),
                    Command::Annotations { command } => state.annotations(command),
                    Command::IsActive => reply(state.is_active()),
                    Command::IsTextEditing => reply(state.is_text_editing()),
                    Command::Exit => return Ok(()),
                }
            }
        }
        state.handle_timeouts(Instant::now());
    }
}
