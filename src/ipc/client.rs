use std::io::ErrorKind::{ConnectionRefused, NotFound};
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

use super::control_socket_addresses;
use crate::cli::Command;

const SOCKET_TIMEOUT: Duration = Duration::from_secs(1);

/// Sends a command and, with `reply`, returns the overlay's response or `None`
/// when the overlay is not running.
pub(crate) fn send(command: &Command, reply: bool) -> Result<Option<String>, String> {
    let addresses = control_socket_addresses()?;
    let socket = UnixDatagram::unbound().map_err(|error| error.to_string())?;
    let missing = |error: &std::io::Error| matches!(error.kind(), ConnectionRefused | NotFound);
    if reply {
        // Credentials make the kernel autobind a unique address for the reply.
        rustix::net::sockopt::set_socket_passcred(&socket, true)
            .map_err(|error| format!("could not enable control socket credentials: {error}"))?;
    }
    let connected = socket
        .set_write_timeout(Some(SOCKET_TIMEOUT))
        .and_then(|()| match socket.connect_addr(&addresses[0]) {
            Err(error) if missing(&error) => socket.connect_addr(&addresses[1]),
            connected => connected,
        });
    match connected {
        Err(error) if reply && missing(&error) => return Ok(None),
        Err(error) => return Err(format!("could not connect to the overlay: {error}")),
        Ok(()) if !reply => {}
        Ok(()) => socket
            .set_read_timeout(Some(SOCKET_TIMEOUT))
            .map_err(|error| format!("could not configure control socket: {error}"))?,
    }
    let not_running = |error: &std::io::Error| reply && error.kind() == ConnectionRefused;
    match socket.send(command.serialize().as_bytes()) {
        Err(error) if not_running(&error) => return Ok(None),
        Err(error) => return Err(format!("could not send command: {error}")),
        Ok(_) if !reply => return Ok(None),
        Ok(_) => {}
    }

    let mut response = [0; crate::cli::MAX_COMMAND_SIZE];
    let size = match socket.recv(&mut response) {
        Ok(size) => size,
        Err(error) if not_running(&error) => return Ok(None),
        Err(error) => return Err(format!("could not receive overlay response: {error}")),
    };
    if size == response.len() {
        return Err("overlay response is too large".into());
    }
    let response =
        String::from_utf8(response[..size].to_vec()).map_err(|_| "invalid overlay response")?;
    if let Some(error) = response.strip_prefix("error:") {
        return Err(error.into());
    }
    Ok(Some(response))
}
