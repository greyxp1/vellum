use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use rustix::event::{PollFd, PollFlags};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
};
use wayland_client::{Connection, Dispatch, QueueHandle};

use super::State;
use crate::draw::{Action, TextInputSnapshot};

const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WRITES: usize = 16;

struct PasteTarget {
    session: u64,
    generation: parley::Generation,
}

impl From<TextInputSnapshot<'_>> for PasteTarget {
    fn from(snapshot: TextInputSnapshot<'_>) -> Self {
        Self {
            session: snapshot.session,
            generation: snapshot.generation,
        }
    }
}

impl PasteTarget {
    fn matches(&self, snapshot: TextInputSnapshot<'_>) -> bool {
        self.session == snapshot.session && self.generation == snapshot.generation
    }
}

struct Paste {
    stream: UnixStream,
    bytes: Vec<u8>,
    target: PasteTarget,
}

struct Send {
    file: File,
    bytes: Arc<[u8]>,
    offset: usize,
}

#[derive(Default)]
pub(super) struct Clipboard {
    pub serial: Option<u32>,
    offers: HashMap<WlDataOffer, Vec<String>>,
    selection: Option<WlDataOffer>,
    source: Option<WlDataSource>,
    paste: Option<Paste>,
    sends: Vec<Send>,
}

impl State {
    pub(crate) fn clipboard_poll_fds(&self) -> impl Iterator<Item = PollFd<'_>> {
        self.clipboard
            .paste
            .iter()
            .map(|paste| PollFd::new(&paste.stream, PollFlags::IN))
            .chain(
                self.clipboard
                    .sends
                    .iter()
                    .map(|send| PollFd::new(&send.file, PollFlags::OUT)),
            )
    }

    pub(super) fn copy_text(&mut self) -> bool {
        let (Some(manager), Some(device), Some(serial), Some(text)) = (
            &self.wayland.data_device_manager,
            &self.wayland.data_device,
            self.clipboard.serial,
            self.draw.clipboard_text(),
        ) else {
            return false;
        };
        if text.is_empty() || text.len() > MAX_TEXT_BYTES {
            return false;
        }
        let source =
            manager.create_data_source(&self.qhandle, Arc::<[u8]>::from(text.into_bytes()));
        for mime in TEXT_TYPES {
            source.offer(mime.to_owned());
        }
        device.set_selection(Some(&source), serial);
        if let Some(previous) = self.clipboard.source.replace(source) {
            previous.destroy();
        }
        true
    }

    pub(super) fn paste_text(&mut self) {
        let (Some(offer), Some(target)) = (
            &self.clipboard.selection,
            self.draw.text_input_snapshot().map(PasteTarget::from),
        ) else {
            return;
        };
        let Some(types) = self.clipboard.offers.get(offer) else {
            return;
        };
        let Some(mime) = TEXT_TYPES
            .iter()
            .find(|mime| types.iter().any(|t| t == **mime))
        else {
            return;
        };
        let receive = || -> std::io::Result<_> {
            let (stream, sender) = UnixStream::pair()?;
            stream.set_nonblocking(true)?;
            offer.receive((*mime).to_owned(), sender.as_fd());
            Ok(Paste {
                stream,
                bytes: Vec::new(),
                target,
            })
        };
        match receive() {
            Ok(paste) => self.clipboard.paste = Some(paste),
            Err(error) => log::warn!("could not receive clipboard: {error}"),
        }
    }

    pub(crate) fn dispatch_clipboard(&mut self) {
        if let Some(paste) = &self.clipboard.paste
            && self
                .draw
                .text_input_snapshot()
                .is_none_or(|snapshot| !paste.target.matches(snapshot))
        {
            self.clipboard.paste = None;
        }
        let mut completed = false;
        if let Some(paste) = &mut self.clipboard.paste {
            let mut buffer = [0; 8192];
            // Bound each dispatch so a large transfer cannot monopolize drawing.
            for _ in 0..32 {
                match paste.stream.read(&mut buffer) {
                    Ok(0) => {
                        completed = true;
                        break;
                    }
                    Ok(size) if paste.bytes.len() + size <= MAX_TEXT_BYTES => {
                        paste.bytes.extend_from_slice(&buffer[..size]);
                    }
                    Ok(_) => {
                        log::warn!("clipboard text exceeds {MAX_TEXT_BYTES} bytes");
                        self.clipboard.paste = None;
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        log::warn!("could not read clipboard: {error}");
                        self.clipboard.paste = None;
                        break;
                    }
                }
            }
        }
        if completed && let Some(paste) = self.clipboard.paste.take() {
            match String::from_utf8(paste.bytes) {
                Ok(text) if !text.is_empty() => self.apply_action(Action::PasteText(text)),
                Ok(_) => {}
                Err(error) => log::warn!("clipboard is not UTF-8 text: {error}"),
            }
        }
        self.clipboard.sends.retain_mut(|send| {
            let end = (send.offset + 256 * 1024).min(send.bytes.len());
            match send.file.write(&send.bytes[send.offset..end]) {
                Ok(size) => {
                    send.offset += size;
                    size > 0 && send.offset < send.bytes.len()
                }
                Err(error) => matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ),
            }
        });
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    wayland_client::event_created_child!(State, WlDataDevice, [0 => (WlDataOffer, ())]);

    fn event(
        state: &mut Self,
        _proxy: &WlDataDevice,
        event: wl_data_device::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                state.clipboard.offers.insert(id, Vec::new());
            }
            wl_data_device::Event::Selection { id } => {
                if let Some(previous) = state.clipboard.selection.take()
                    && Some(&previous) != id.as_ref()
                {
                    state.clipboard.offers.remove(&previous);
                    previous.destroy();
                }
                state.clipboard.selection = id;
            }
            wl_data_device::Event::Enter {
                serial,
                id: Some(offer),
                ..
            } => {
                offer.accept(serial, None);
                state.clipboard.offers.remove(&offer);
                offer.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &WlDataOffer,
        event: wl_data_offer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Some(types) = state.clipboard.offers.get_mut(proxy)
        {
            types.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, Arc<[u8]>> for State {
    fn event(
        state: &mut Self,
        proxy: &WlDataSource,
        event: wl_data_source::Event,
        bytes: &Arc<[u8]>,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd }
                if TEXT_TYPES.contains(&mime_type.as_str())
                    && state.clipboard.sends.len() < MAX_WRITES =>
            {
                let file = File::from(fd);
                match fcntl_getfl(&file)
                    .and_then(|flags| fcntl_setfl(&file, flags | OFlags::NONBLOCK))
                {
                    Ok(()) => state.clipboard.sends.push(Send {
                        file,
                        bytes: bytes.clone(),
                        offset: 0,
                    }),
                    Err(error) => log::warn!("could not send clipboard: {error}"),
                }
            }
            wl_data_source::Event::Cancelled => {
                if state.clipboard.source.as_ref() == Some(proxy) {
                    state.clipboard.source = None;
                }
                proxy.destroy();
            }
            _ => {}
        }
    }
}
