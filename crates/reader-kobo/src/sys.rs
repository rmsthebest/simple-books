//! The system calls with no safe wrapper in `std`: mapping the framebuffer,
//! the hwtcon and evdev ioctls, and `poll`. Everything `unsafe` in this
//! crate is here.
//!
//! The hwtcon definitions are from FBInk's `eink/mtk-kobo.h`, as recorded in
//! `docs/clara-bw-hardware.md`.
#![allow(unsafe_code, reason = "the system calls std doesn't wrap")]

use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::{AsRawFd, BorrowedFd},
    time::Duration,
};

/// `_IOC(dir, type, nr, size)` from the kernel's `asm-generic/ioctl.h`.
const fn ioc(dir: u32, kind: u8, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((kind as u32) << 8) | nr as u32
}
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const FBIOGET_VSCREENINFO: u32 = 0x4600;
const FBIOGET_FSCREENINFO: u32 = 0x4602;
const HWTCON_SEND_UPDATE: u32 = ioc(IOC_WRITE, b'F', 0x2E, size_of::<UpdateData>());
const HWTCON_WAIT_FOR_UPDATE_COMPLETE: u32 =
    ioc(IOC_READ | IOC_WRITE, b'F', 0x2F, size_of::<UpdateMarker>());
const EVIOCGRAB: u32 = ioc(IOC_WRITE, b'E', 0x90, size_of::<libc::c_int>());

/// `struct hwtcon_update_data`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct UpdateData {
    top: u32,
    left: u32,
    width: u32,
    height: u32,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    flags: u32,
    dither_mode: i32,
}

/// `struct hwtcon_update_marker_data`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct UpdateMarker {
    marker: u32,
    collision_test: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waveform {
    /// Grey, for page turns.
    Gl16,
    /// Full grey with a flash.
    Gc16,
}

/// `/dev/fb0`, mapped.
pub(crate) struct Framebuffer {
    file: File,
    map: *mut u8,
    len: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    next_marker: u32,
    /// The update sent last, until it's known to have finished.
    pending: Option<u32>,
}

impl std::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Framebuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .finish_non_exhaustive()
    }
}

/// `fb_fix_screeninfo`'s `smem_len` and `line_length`, which move with the
/// size of `unsigned long`.
const FIX_SMEM_LEN: usize = 16 + size_of::<usize>();
const FIX_LINE_LENGTH: usize = FIX_SMEM_LEN + 4 * 4 + 3 * 2 + 2;

impl Framebuffer {
    pub(crate) fn open() -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        // fb_var_screeninfo is 40 u32s; xres, yres and bits_per_pixel are
        // the first, second and seventh.
        let mut var = [0u32; 40];
        // SAFETY: FBIOGET_VSCREENINFO writes one fb_var_screeninfo, which
        // is 160 bytes, into the buffer, which is that size.
        check(unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                request(FBIOGET_VSCREENINFO),
                var.as_mut_ptr(),
            )
        })?;
        let mut fix = [0u8; 128];
        // SAFETY: fb_fix_screeninfo is 68 bytes on 32-bit and 80 on 64-bit;
        // the buffer is bigger than either.
        check(unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                request(FBIOGET_FSCREENINFO),
                fix.as_mut_ptr(),
            )
        })?;
        let read_u32 =
            |at: usize| u32::from_ne_bytes([fix[at], fix[at + 1], fix[at + 2], fix[at + 3]]);
        let (width, height, bits) = (var[0], var[1], var[6]);
        let (len, stride) = (read_u32(FIX_SMEM_LEN) as usize, read_u32(FIX_LINE_LENGTH));
        if bits != 8 || stride < width || len < stride as usize * height as usize {
            return Err(io::Error::other(format!(
                "unexpected framebuffer: {width}×{height}, {bits} bits, stride {stride}, {len} bytes"
            )));
        }
        // SAFETY: a shared mapping of the framebuffer's memory, the size
        // the driver reports. It stays mapped until drop.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file,
            map: map.cast(),
            len,
            width,
            height,
            stride,
            next_marker: 1,
            pending: None,
        })
    }

    /// The mapped pixels, `stride` bytes a row.
    pub(crate) fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: `map` is `len` bytes, mapped read-write until drop, and
        // `&mut self` keeps this the only slice of it.
        unsafe { std::slice::from_raw_parts_mut(self.map, self.len) }
    }

    /// Asks the panel to redraw the whole screen from the framebuffer.
    pub(crate) fn update(&mut self, waveform: Waveform) -> io::Result<()> {
        let marker = self.next_marker;
        self.next_marker = self.next_marker.wrapping_add(1).max(1);
        let (waveform_mode, update_mode) = match waveform {
            Waveform::Gl16 => (3, 0),
            Waveform::Gc16 => (2, 1),
        };
        let data = UpdateData {
            top: 0,
            left: 0,
            width: self.width,
            height: self.height,
            waveform_mode,
            update_mode,
            update_marker: marker,
            flags: 0,
            dither_mode: 0,
        };
        // SAFETY: HWTCON_SEND_UPDATE reads one hwtcon_update_data.
        check(unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                request(HWTCON_SEND_UPDATE),
                &raw const data,
            )
        })?;
        self.pending = Some(marker);
        Ok(())
    }

    /// Waits until the last update has finished, so the framebuffer can be
    /// written again without tearing what the panel is still drawing.
    pub(crate) fn wait(&mut self) -> io::Result<()> {
        let Some(marker) = self.pending.take() else {
            return Ok(());
        };
        let mut data = UpdateMarker {
            marker,
            collision_test: 0,
        };
        // SAFETY: HWTCON_WAIT_FOR_UPDATE_COMPLETE reads and writes one
        // hwtcon_update_marker_data.
        check(unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                request(HWTCON_WAIT_FOR_UPDATE_COMPLETE),
                &raw mut data,
            )
        })
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what `open` mapped, once.
        unsafe {
            libc::munmap(self.map.cast(), self.len);
        }
    }
}

/// Keeps an input device's events from everyone else until the file closes.
pub(crate) fn grab(device: &File) -> io::Result<()> {
    let on: libc::c_int = 1;
    // SAFETY: EVIOCGRAB takes an int by value, passed as the argument.
    check(unsafe { libc::ioctl(device.as_raw_fd(), request(EVIOCGRAB), on) })
}

/// Waits until one of `files` can be read, or `timeout` passes. Returns
/// which are readable.
pub(crate) fn poll(files: &[BorrowedFd<'_>], timeout: Duration) -> io::Result<Vec<bool>> {
    let mut fds: Vec<libc::pollfd> = files
        .iter()
        .map(|fd| libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `fds` is a valid array of `fds.len()` pollfds for the call.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(vec![false; files.len()]);
        }
        return Err(error);
    }
    Ok(fds
        .iter()
        .map(|fd| fd.revents & libc::POLLIN != 0)
        .collect())
}

/// Flushes every filesystem, before sleeping.
pub(crate) fn sync() {
    // SAFETY: sync takes no arguments and can't fail.
    unsafe { libc::sync() }
}

/// An ioctl number as the C library takes it: a signed int on musl, the
/// Kobo build, and an unsigned long on glibc. Its bits are what matter.
#[allow(
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    reason = "the request is a bit pattern, whatever its C type"
)]
const fn request(number: u32) -> libc::Ioctl {
    number as libc::Ioctl
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_the_kernel_headers() {
        // hwtcon_update_data is nine 32-bit fields.
        assert_eq!(size_of::<UpdateData>(), 36);
        assert_eq!(HWTCON_SEND_UPDATE, 0x4024_462E);
        assert_eq!(HWTCON_WAIT_FOR_UPDATE_COMPLETE, 0xC008_462F);
        assert_eq!(EVIOCGRAB, 0x4004_4590);
    }

    #[test]
    fn fix_screeninfo_offsets_follow_the_word_size() {
        if size_of::<usize>() == 4 {
            assert_eq!((FIX_SMEM_LEN, FIX_LINE_LENGTH), (20, 44));
        } else {
            assert_eq!((FIX_SMEM_LEN, FIX_LINE_LENGTH), (24, 48));
        }
    }
}
