use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use gbm::{BufferObject, BufferObjectFlags, Device, Format, Modifier};
use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_shm, wl_shm::WlShm};

use super::super::State;

/// Returns the buffer with its memfd, which the compositor maps but Vellum does not.
pub fn create_shm_buffer(
    shm: &WlShm,
    qh: &QueueHandle<State>,
    width: u32,
    height: u32,
    format: wl_shm::Format,
) -> Result<(WlBuffer, OwnedFd), String> {
    let bytes_per_pixel = match format {
        wl_shm::Format::Argb8888
        | wl_shm::Format::Xrgb8888
        | wl_shm::Format::Abgr8888
        | wl_shm::Format::Xbgr8888 => 4,
        wl_shm::Format::Rgb888 | wl_shm::Format::Bgr888 => 3,
        _ => return Err(format!("unsupported capture format: {format:?}")),
    };
    let stride = u64::from(width) * bytes_per_pixel;
    let size = i32::try_from(stride * u64::from(height))
        .map_err(|_| "capture buffer is too large".to_owned())?;
    let fd = memfd_create(
        "vellum-capture",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )
    .map_err(|error| format!("create capture memfd: {error}"))?;
    ftruncate(&fd, size as u64).map_err(|error| format!("resize capture memfd: {error}"))?;
    // Size seals prevent truncation while the compositor maps the buffer.
    fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL)
        .map_err(|error| format!("seal capture memfd: {error}"))?;
    let pool = shm.create_pool(fd.as_fd(), size, qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        format,
        qh,
        (),
    );
    pool.destroy();
    Ok((buffer, fd))
}

pub struct DmabufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

pub struct DmabufBuffer {
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
    let matches = |metadata: std::fs::Metadata| {
        metadata.file_type().is_char_device()
            && (metadata.rdev() == device
                || target.is_some() && sysfs_device(metadata.rdev()) == target)
    };
    std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .filter(|entry| entry.file_name().as_encoded_bytes().starts_with(b"renderD"))
        .filter_map(|entry| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(entry.path())
                .ok()
        })
        .find(|file| file.metadata().is_ok_and(matches))
}
