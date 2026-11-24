use std::borrow::Cow;
use std::time::Instant;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::backend::WaylandError;

use crate::cli::{Command, MAX_COMMAND_SIZE};
use crate::config::{Config, Settings};
use crate::ipc::ControlSocket;
use crate::state::{self, State};

// Yield to drawing, input and the other endpoint even when a queue stays full.
const MAX_SOCKET_BATCH: usize = 32;

pub(crate) fn run(mut settings: Settings, mut config: Config) -> Result<(), String> {
    let control = ControlSocket::bind()?;
    let sockets = &control.sockets;

    let (mut state, mut event_queue) = state::State::setup_wayland(&settings)?;
    let mut pending_settings = None;

    loop {
        event_queue
            .dispatch_pending(&mut state)
            .map_err(|error| format!("Wayland dispatch failed: {error}"))?;
        state.dispatch_clipboard();
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
            let mut fds = vec![
                PollFd::new(
                    &event_queue,
                    PollFlags::IN
                        | if flush_blocked {
                            PollFlags::OUT
                        } else {
                            PollFlags::empty()
                        },
                ),
                PollFd::new(&sockets[0], PollFlags::IN),
                PollFd::new(&sockets[1], PollFlags::IN),
                config.fd().map_or_else(
                    || PollFd::new(&sockets[0], PollFlags::empty()),
                    |fd| PollFd::new(fd, PollFlags::IN),
                ),
            ];
            fds.extend(state.clipboard_poll_fds());
            if let Err(error) = poll(&mut fds, timeout.as_ref()) {
                if error == rustix::io::Errno::INTR {
                    continue;
                }
                return Err(format!("event polling failed: {error}"));
            }
            (
                fds[0].revents().contains(PollFlags::IN),
                [1, 2].map(|index| fds[index].revents().contains(PollFlags::IN)),
                fds[3].revents().contains(PollFlags::IN),
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

        for (socket, ready) in sockets.iter().zip(socket_ready) {
            if !ready {
                continue;
            }
            let mut message = [0; MAX_COMMAND_SIZE + 1];
            for _ in 0..MAX_SOCKET_BATCH {
                let (size, sender) = match ControlSocket::recv(socket, &mut message) {
                    Ok(Some(message)) => message,
                    Ok(None) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(format!("socket read failed: {error}")),
                };
                let response = if size > MAX_COMMAND_SIZE {
                    log::warn!("socket message exceeded {MAX_COMMAND_SIZE} bytes");
                    Some(Err("command is too large".into()))
                } else {
                    match Command::deserialize(&message[..size]) {
                        Ok(Command::Exit) => return Ok(()),
                        Ok(command) => handle(&mut state, command),
                        Err(error) => {
                            log::warn!("ignored socket message: {error}");
                            Some(Err(error.into()))
                        }
                    }
                };
                if let (Some(response), Some(sender)) = (response, &sender) {
                    let response = response.unwrap_or_else(|error| format!("error:{error}").into());
                    let flags = rustix::net::SendFlags::empty();
                    if let Err(error) =
                        rustix::net::sendto(socket, response.as_bytes(), flags, sender)
                    {
                        log::warn!("could not send response: {error}");
                    }
                }
            }
        }
        state.handle_timeouts(Instant::now());
    }
}

/// Applies a command and returns the reply for commands that expect one.
fn handle(state: &mut State, command: Command) -> Option<Result<Cow<'static, str>, String>> {
    let ok = |result: Result<(), String>| Some(result.map(|()| "ok".into()));
    match command {
        Command::Use { tool } => return ok(state.select_tool(tool)),
        Command::Set { tool, setting } => return ok(state.set_tool_setting(tool, setting)),
        Command::Status {
            field, tool, json, ..
        } => return Some(state.status_reply(field, tool, json)),
        Command::Toggle => state.set_input_active(!state.is_active()),
        Command::Activate => state.set_input_active(true),
        Command::Deactivate => state.set_input_active(false),
        Command::Clear { deactivate } => {
            state.clear();
            if deactivate {
                state.set_input_active(false);
            }
        }
        Command::Whiteboard(command) => state.whiteboard(command),
        Command::Presentation(visibility) => state.set_presentation(visibility),
        Command::Annotations(visibility) => state.annotations(visibility),
        // The event loop stops before handling exit.
        Command::Exit => {}
    }
    None
}
