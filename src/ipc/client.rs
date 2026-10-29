use std::io::ErrorKind;
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

use super::control_socket_path;
use crate::cli::Command;

pub(crate) fn send_command(command: &Command) -> Result<(), String> {
    let path = control_socket_path()?;
    let socket = UnixDatagram::unbound().map_err(|error| error.to_string())?;
    socket
        .connect(path)
        .map_err(|error| format!("could not connect to the overlay: {error}"))?;

    let message = command.serialize();
    socket
        .send(message.as_bytes())
        .map_err(|error| format!("could not send command: {error}"))?;
    Ok(())
}

pub(crate) fn query(request: &Command) -> Result<bool, String> {
    let path = control_socket_path()?;
    let socket = UnixDatagram::unbound().map_err(|error| error.to_string())?;
    // Credentials make the kernel autobind a unique address for the reply.
    rustix::net::sockopt::set_socket_passcred(&socket, true)
        .map_err(|error| format!("could not enable control socket credentials: {error}"))?;
    if let Err(error) = socket.connect(path) {
        if matches!(
            error.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::NotFound
        ) {
            return Ok(false);
        }
        return Err(format!("could not connect to the overlay: {error}"));
    }
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|error| format!("could not configure control socket: {error}"))?;
    let request = request.serialize();
    if let Err(error) = socket.send(request.as_bytes()) {
        if error.kind() == ErrorKind::ConnectionRefused {
            return Ok(false);
        }
        return Err(format!("could not send command: {error}"));
    }

    let mut response = [0; 5];
    let size = match socket.recv(&mut response) {
        Ok(size) => size,
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
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
