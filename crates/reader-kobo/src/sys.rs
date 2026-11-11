//! The system calls with no safe wrapper in `std`: mapping the framebuffer,
//! the hwtcon and evdev ioctls, the kernel's device events, and `poll`. Everything `unsafe` in this
//! crate is here.
//!
//! The hwtcon definitions are from FBInk's `eink/mtk-kobo.h`, as recorded in
//! `docs/clara-bw-hardware.md`.
#![allow(unsafe_code, reason = "the system calls std doesn't wrap")]

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    time::Duration,
};

/// `_IOC(dir, type, nr, size)` from the kernel's `asm-generic/ioctl.h`.
const fn ioc(dir: u32, kind: u8, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((kind as u32) << 8) | nr as u32
}
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const FBIOGET_VSCREENINFO: u32 = 0x4600;
const FBIOPUT_VSCREENINFO: u32 = 0x4601;
const FBIOGET_FSCREENINFO: u32 = 0x4602;
const HWTCON_SEND_UPDATE: u32 = ioc(IOC_WRITE, b'F', 0x2E, size_of::<UpdateData>());
const HWTCON_WAIT_FOR_UPDATE_COMPLETE: u32 =
    ioc(IOC_READ | IOC_WRITE, b'F', 0x2F, size_of::<UpdateMarker>());
const EVIOCGRAB: u32 = ioc(IOC_WRITE, b'E', 0x90, size_of::<libc::c_int>());
/// Bytes in a key bitmap: one bit for each code up to `KEY_MAX`, 0x2ff.
const KEY_BYTES: usize = 0x300 / 8;
const EVIOCGKEY: u32 = ioc(IOC_READ, b'E', 0x18, KEY_BYTES);

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

/// `fb_var_screeninfo`, as 40 `u32`s. These are the fields used here.
type VarInfo = [u32; 40];
const VAR_BITS: usize = 6;
const VAR_GRAYSCALE: usize = 7;
const VAR_ACTIVATE: usize = 21;
const VAR_ROTATE: usize = 34;
/// Apply the settings even if the driver thinks nothing changed.
const FB_ACTIVATE_FORCE: u32 = 128;

/// The screen as the reader draws it: one byte a pixel, gray, upright.
/// KOReader sets the same, and these are the values it leaves. Nickel runs
/// at 32 bits a pixel.
const READER_BITS: u32 = 8;
const READER_GRAYSCALE: u32 = 1;
const READER_ROTATE: u32 = 3;

/// Nickel's screen settings while the reader has changed them, so that
/// `reader-kobo --restore-screen` can put them back after a crash.
pub(crate) const SAVED_SCREEN: &str = "/tmp/simple-books-screen";

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
    /// The settings found on opening, to restore on drop, if they had to
    /// be changed.
    original: Option<VarInfo>,
}

impl std::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Framebuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("original", &self.original.map(|v| Settings::of(&v)))
            .finish_non_exhaustive()
    }
}

/// `fb_fix_screeninfo`'s `smem_len` and `line_length`, which move with the
/// size of `unsigned long`.
const FIX_SMEM_LEN: usize = 16 + size_of::<usize>();
const FIX_LINE_LENGTH: usize = FIX_SMEM_LEN + 4 * 4 + 3 * 2 + 2;

/// The screen settings that matter here, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Settings {
    bits: u32,
    grayscale: u32,
    rotate: u32,
}

impl Settings {
    fn of(var: &VarInfo) -> Self {
        Self {
            bits: var[VAR_BITS],
            grayscale: var[VAR_GRAYSCALE],
            rotate: var[VAR_ROTATE],
        }
    }
}

fn get_var(file: &File) -> io::Result<VarInfo> {
    let mut var = [0u32; 40];
    // SAFETY: FBIOGET_VSCREENINFO writes one fb_var_screeninfo, which is
    // 160 bytes, into the buffer, which is that size.
    check(unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            request(FBIOGET_VSCREENINFO),
            var.as_mut_ptr(),
        )
    })?;
    Ok(var)
}

/// Sets the screen's settings. MediaTek's driver ignores a change of depth
/// unless the display path's source format is changed to match first, as
/// FBInk does in `fbink_set_fb_info`.
fn put_var(file: &File, var: &VarInfo) -> io::Result<()> {
    set_source_format(var[VAR_BITS])?;
    let mut var = *var;
    var[VAR_ACTIVATE] = FB_ACTIVATE_FORCE;
    // SAFETY: FBIOPUT_VSCREENINFO reads and writes one fb_var_screeninfo,
    // which is the buffer's size.
    check(unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            request(FBIOPUT_VSCREENINFO),
            var.as_mut_ptr(),
        )
    })
}

/// Writes the hwtcon display path's source pixel format for a depth: `Y8`
/// for 8 bits and `ABGR32` for 32, the formats its kernel offers, with the
/// trailing NUL FBInk writes. Nothing for other depths, or without hwtcon.
fn set_source_format(bits: u32) -> io::Result<()> {
    let format: &[u8] = match bits {
        8 => b"Y8\0",
        32 => b"ABGR32\0",
        _ => return Ok(()),
    };
    for entry in std::fs::read_dir("/sys/devices/platform")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().contains("hwtcon") {
            let path = entry.path().join("mdp_src_format");
            if path.exists() {
                // One write, as FBInk does: the driver takes the name and
                // reports the NUL unwritten, and a second write of just the
                // NUL is an error.
                let written = OpenOptions::new().write(true).open(path)?.write(format)?;
                if written + 1 < format.len() {
                    return Err(io::Error::other("hwtcon took part of the format name"));
                }
            }
        }
    }
    Ok(())
}

/// Puts back the screen settings a reader that didn't exit cleanly left
/// changed. Does nothing if there are none saved.
pub(crate) fn restore_screen() -> io::Result<()> {
    let Ok(bytes) = std::fs::read(SAVED_SCREEN) else {
        return Ok(());
    };
    let mut var = [0u32; 40];
    if bytes.len() != size_of::<VarInfo>() {
        return Err(io::Error::other(format!(
            "{SAVED_SCREEN} is the wrong size"
        )));
    }
    for (field, bytes) in var.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *field = u32::from_ne_bytes(*bytes);
    }
    let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
    put_var(&file, &var)?;
    std::fs::remove_file(SAVED_SCREEN)
}

impl Framebuffer {
    pub(crate) fn open() -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        let found = get_var(&file)?;
        let wanted = Settings {
            bits: READER_BITS,
            grayscale: READER_GRAYSCALE,
            rotate: READER_ROTATE,
        };
        let original = if Settings::of(&found) == wanted {
            None
        } else {
            let bytes: Vec<u8> = found.iter().flat_map(|f| f.to_ne_bytes()).collect();
            std::fs::write(SAVED_SCREEN, bytes)?;
            let mut var = found;
            var[VAR_BITS] = wanted.bits;
            var[VAR_GRAYSCALE] = wanted.grayscale;
            var[VAR_ROTATE] = wanted.rotate;
            put_var(&file, &var)?;
            Some(found)
        };
        let framebuffer = Self::map(file, original);
        if framebuffer.is_err()
            && let Some(original) = &original
        {
            // Leave the screen as it was found.
            let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
            if put_var(&file, original).is_ok() {
                let _ = std::fs::remove_file(SAVED_SCREEN);
            }
        }
        framebuffer
    }

    /// Maps the framebuffer as it's now set up.
    fn map(file: File, original: Option<VarInfo>) -> io::Result<Self> {
        let var = get_var(&file)?;
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
        let (width, height, bits) = (var[0], var[1], var[VAR_BITS]);
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
            original,
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
        if let Some(original) = &self.original
            && put_var(&self.file, original).is_ok()
        {
            let _ = std::fs::remove_file(SAVED_SCREEN);
        }
    }
}

/// Keeps an input device's events from everyone else until the file closes.
pub(crate) fn grab(device: &File) -> io::Result<()> {
    let on: libc::c_int = 1;
    // SAFETY: EVIOCGRAB takes an int by value, passed as the argument.
    check(unsafe { libc::ioctl(device.as_raw_fd(), request(EVIOCGRAB), on) })
}

/// Whether key or button `code` on an input device is down now.
pub(crate) fn key_down(device: &File, code: u16) -> io::Result<bool> {
    let mut keys = [0u8; KEY_BYTES];
    // SAFETY: EVIOCGKEY writes at most its size, KEY_BYTES, into `keys`.
    check(unsafe { libc::ioctl(device.as_raw_fd(), request(EVIOCGKEY), keys.as_mut_ptr()) })?;
    let code = usize::from(code);
    Ok(keys
        .get(code / 8)
        .is_some_and(|byte| byte & (1 << (code % 8)) != 0))
}

/// A socket that's readable whenever the kernel reports a device change,
/// such as a USB cable going in: the uevents udev hears, for the reader to
/// look at sysfs again. Non-blocking; read it empty each time.
pub(crate) fn uevents() -> io::Result<File> {
    // SAFETY: plain socket creation; the result is checked before use.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_KOBJECT_UEVENT,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a new socket that nothing else owns.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: an all-zero sockaddr_nl is valid; the fields that matter are
    // set below.
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // The kernel's uevent group.
    address.nl_groups = 1;
    // SAFETY: `address` is a sockaddr_nl, and the length says so.
    check(unsafe {
        libc::bind(
            socket.as_raw_fd(),
            std::ptr::from_ref(&address).cast(),
            size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    })?;
    Ok(File::from(socket))
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
/// The space free and in all, in bytes, on the filesystem holding `path`.
#[allow(
    clippy::useless_conversion,
    reason = "the field types differ between the Kobo and a 64-bit computer"
)]
pub(crate) fn space(path: &str) -> io::Result<(u64, u64)> {
    let path = std::ffi::CString::new(path).map_err(io::Error::other)?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a valid C string and `stats` has room for the
    // answer, which is only read once the call says it's filled.
    let stats = unsafe {
        if libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        stats.assume_init()
    };
    let block = u64::from(stats.f_frsize);
    Ok((
        u64::from(stats.f_bavail) * block,
        u64::from(stats.f_blocks) * block,
    ))
}

pub(crate) fn sync() {
    // SAFETY: sync takes no arguments and can't fail.
    unsafe { libc::sync() }
}

/// Closes every descriptor past stderr and leaves for `/`, as the helper
/// started from inside Nickel does first. It inherits Nickel's open files
/// and working directory, and any of them on `/mnt/onboard` would keep
/// Nickel from unmounting it to hand it to a computer over USB.
pub(crate) fn leave_nickels_files() {
    let fds: Vec<libc::c_int> = std::fs::read_dir("/proc/self/fd")
        .map(|dir| {
            dir.flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
                .filter(|fd| *fd > 2)
                .collect()
        })
        .unwrap_or_default();
    for fd in fds {
        // SAFETY: nothing in this process owns these yet: they came with
        // it. The one read_dir used is already closed, and closing it
        // again only fails.
        unsafe {
            libc::close(fd);
        }
    }
    let _ = std::env::set_current_dir("/");
}

/// Makes `command` start in a session of its own, so it outlives whoever
/// started it and gets no signals meant for them.
pub(crate) fn own_session(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe, as calls between fork and exec
    // must be.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// Takes an exclusive lock on `file` if no one holds one, without waiting.
/// It's let go when the file is closed.
pub(crate) fn try_lock(file: &File) -> bool {
    // SAFETY: flock on a descriptor this File owns.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
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
        assert_eq!(EVIOCGKEY, 0x8060_4518);
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
