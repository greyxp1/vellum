use std::io::ErrorKind;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::time::Duration;

use super::control_socket_addresses;
use crate::cli::Command;

const SOCKET_TIMEOUT: Duration = Duration::from_secs(1);

fn connect(socket: &UnixDatagram, addresses: &[SocketAddr; 2]) -> std::io::Result<()> {
    socket.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    socket.connect_addr(&addresses[0]).or_else(|error| {
        if matches!(
            error.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::NotFound
        ) {
            socket.connect_addr(&addresses[1])
        } else {
            Err(error)
        }
    })
}

pub(crate) fn send_command(command: &Command) -> Result<(), String> {
    let addresses = control_socket_addresses()?;
    let socket = UnixDatagram::unbound().map_err(|error| error.to_string())?;
    connect(&socket, &addresses)
        .map_err(|error| format!("could not connect to the overlay: {error}"))?;

    let message = command.serialize();
    socket
        .send(message.as_bytes())
        .map_err(|error| format!("could not send command: {error}"))?;
    Ok(())
}

pub(crate) fn query(request: &Command) -> Result<Option<String>, String> {
    let addresses = control_socket_addresses()?;
    let socket = UnixDatagram::unbound().map_err(|error| error.to_string())?;
    // Credentials make the kernel autobind a unique address for the reply.
    rustix::net::sockopt::set_socket_passcred(&socket, true)
        .map_err(|error| format!("could not enable control socket credentials: {error}"))?;
    if let Err(error) = connect(&socket, &addresses) {
        if matches!(
            error.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::NotFound
        ) {
            return Ok(None);
        }
        return Err(format!("could not connect to the overlay: {error}"));
    }
    socket
        .set_read_timeout(Some(SOCKET_TIMEOUT))
        .map_err(|error| format!("could not configure control socket: {error}"))?;
    let request = request.serialize();
    if let Err(error) = socket.send(request.as_bytes()) {
        if error.kind() == ErrorKind::ConnectionRefused {
            return Ok(None);
        }
        return Err(format!("could not send command: {error}"));
    }

    let mut response = [0; 4096];
    let size = match socket.recv(&mut response) {
        Ok(size) => size,
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
            return Ok(None);
        }
        Err(error) => return Err(format!("could not receive overlay status: {error}")),
    };
    if size == response.len() {
        return Err("overlay status is too large".into());
    }
    String::from_utf8(response[..size].to_vec())
        .map(Some)
        .map_err(|_| "invalid overlay status".into())
}
