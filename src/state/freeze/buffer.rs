use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use gbm::{BufferObject, BufferObjectFlags, Device, Format, Modifier};
use memmap2::MmapMut;
use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_shm, wl_shm::WlShm};

use super::super::State;

pub struct ShmBuffer {
    pub width: u32,
    pub height: u32,
    pub is_bgra: bool,
    pub has_alpha_channel: bool,
    pub mmap: MmapMut,
    fd: OwnedFd,
    format: wl_shm::Format,
    stride: i32,
    size: i32,
}

impl ShmBuffer {
    pub fn new(width: u32, height: u32, format: wl_shm::Format) -> Result<Self, String> {
        let (is_bgra, has_alpha_channel, bytes_per_pixel) = match format {
            wl_shm::Format::Argb8888 => (true, true, 4),
            wl_shm::Format::Xrgb8888 => (true, false, 4),
            wl_shm::Format::Abgr8888 => (false, true, 4),
            wl_shm::Format::Xbgr8888 => (false, false, 4),
            wl_shm::Format::Rgb888 => (false, false, 3),
            wl_shm::Format::Bgr888 => (false, false, 3),
            _ => return Err(format!("unsupported capture format: {format:?}")),
        };
        let stride = width
            .checked_mul(bytes_per_pixel)
            .and_then(|n| i32::try_from(n).ok());
        let size = width
            .checked_mul(bytes_per_pixel)
            .and_then(|n| n.checked_mul(height))
            .and_then(|n| i32::try_from(n).ok());
        let (Some(stride), Some(size)) = (stride, size) else {
            return Err("capture buffer is too large".into());
        };
        if width == 0 || height == 0 {
            return Err("capture buffer has zero dimensions".into());
        }
        let fd = memfd_create(
            "vellum-capture",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )
        .map_err(|error| format!("create capture memfd: {error}"))?;
        ftruncate(&fd, size as u64).map_err(|error| format!("resize capture memfd: {error}"))?;
        fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL)
            .map_err(|error| format!("seal capture memfd: {error}"))?;
        // Size seals prevent truncation while either process uses the shared mapping.
        let mmap = unsafe { MmapMut::map_mut(&fd) }
            .map_err(|error| format!("map capture memfd: {error}"))?;
        Ok(Self {
            width,
            height,
            is_bgra,
            has_alpha_channel,
            mmap,
            fd,
            format,
            stride,
            size,
        })
    }

    pub fn create_buffer(&self, shm: &WlShm, qh: &QueueHandle<State>) -> WlBuffer {
        let pool = shm.create_pool(self.fd.as_fd(), self.size, qh, ());
        let buffer = pool.create_buffer(
            0,
            self.width as i32,
            self.height as i32,
            self.stride,
            self.format,
            qh,
            (),
        );
        pool.destroy();
        buffer
    }

    pub fn pixel_data(&mut self) -> Cow<'_, [u8]> {
        match self.format {
            wl_shm::Format::Argb8888
            | wl_shm::Format::Xrgb8888
            | wl_shm::Format::Abgr8888
            | wl_shm::Format::Xbgr8888 => {
                if !self.has_alpha_channel {
                    for pixel in self.mmap.as_chunks_mut::<4>().0 {
                        pixel[3] = 255;
                    }
                }
                Cow::Borrowed(&self.mmap)
            }
            // On little-endian systems RGB888 is stored as B, G, R, while
            // BGR888 is stored as R, G, B.
            wl_shm::Format::Rgb888 => Cow::Owned(
                self.mmap
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255])
                    .collect(),
            ),
            wl_shm::Format::Bgr888 => Cow::Owned(
                self.mmap
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
                    .collect(),
            ),
            _ => unreachable!("ShmBuffer only supports known formats"),
        }
    }
}

pub struct DmabufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

pub struct DmabufBuffer {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub modifier: u64,
    pub planes: Vec<DmabufPlane>,
    // The BO retains the native device, but not its backing file descriptor.
    // Drop the BO before the device and its file.
    _bo: BufferObject<()>,
    _device: Device<File>,
}

pub fn allocate_dmabuf(
    device: u64,
    width: u32,
    height: u32,
    formats: &[(u32, Vec<u64>)],
) -> Option<DmabufBuffer> {
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return None;
    }
    let device = Device::new(open_render_node(device)?).ok()?;
    let preferred = [
        Format::Xrgb8888,
        Format::Argb8888,
        Format::Xbgr8888,
        Format::Abgr8888,
    ];
    for format in preferred {
        for (_, modifiers) in formats.iter().filter(|(code, _)| *code == format as u32) {
            let implicit = modifiers.is_empty();
            let modifiers: Vec<_> = modifiers
                .iter()
                .copied()
                .filter(|&modifier| Modifier::from(modifier) != Modifier::Invalid)
                .collect();
            // An explicit INVALID modifier is not an allocatable layout.
            if modifiers.is_empty() && !implicit {
                continue;
            }
            let bo = if modifiers.is_empty() {
                device.create_buffer_object::<()>(
                    width,
                    height,
                    format,
                    BufferObjectFlags::RENDERING,
                )
            } else {
                device.create_buffer_object_with_modifiers2::<()>(
                    width,
                    height,
                    format,
                    modifiers.iter().copied().map(Modifier::from),
                    BufferObjectFlags::RENDERING,
                )
            };
            let Ok(bo) = bo else { continue };
            let modifier = bo.modifier().into();
            if !modifiers.is_empty() && !modifiers.contains(&modifier) {
                continue;
            }
            let plane_count = bo.plane_count();
            if !(1..=4).contains(&plane_count) {
                continue;
            }
            let planes: Option<Vec<_>> = (0..plane_count as i32)
                .map(|plane| {
                    let stride = bo.stride_for_plane(plane);
                    (stride != 0).then_some(())?;
                    Some(DmabufPlane {
                        fd: bo.fd_for_plane(plane).ok()?,
                        offset: bo.offset(plane),
                        stride,
                    })
                })
                .collect();
            let Some(planes) = planes else { continue };
            return Some(DmabufBuffer {
                width,
                height,
                format: format as u32,
                modifier,
                planes,
                _bo: bo,
                _device: device,
            });
        }
    }
    None
}

fn open_render_node(device: u64) -> Option<File> {
    let sysfs_device = |dev| {
        std::fs::canonicalize(format!(
            "/sys/dev/char/{}:{}/device",
            rustix::fs::major(dev),
            rustix::fs::minor(dev)
        ))
        .ok()
    };
    let target = sysfs_device(device);
    for entry in std::fs::read_dir("/dev/dri").ok()?.flatten() {
        if !entry.file_name().as_encoded_bytes().starts_with(b"renderD") {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).write(true).open(entry.path()) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if metadata.file_type().is_char_device()
            && (metadata.rdev() == device
                || target
                    .as_ref()
                    .is_some_and(|target| sysfs_device(metadata.rdev()).as_ref() == Some(target)))
        {
            return Some(file);
        }
    }
    None
}
