//! Phase 0 draw test for the Kobo Clara BW: draws through hwtcon, times each
//! refresh mode, and maps touch coordinates to screen coordinates. It grabs the
//! touch panel while running so KOReader doesn't react; the grab ends on exit.

use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

const W: usize = 1072;
const H: usize = 1448;
const STRIDE: usize = 1072;

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr as u32
}
const IOW: u32 = 1;
const IOWR: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    top: u32,
    left: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
struct UpdateData {
    region: Rect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    flags: u32,
    dither_mode: i32,
}

#[repr(C)]
struct MarkerData {
    update_marker: u32,
    collision_test: u32,
}

const SEND_UPDATE: u32 = ioc(IOW, b'F', 0x2E, size_of::<UpdateData>());
const WAIT_COMPLETE: u32 = ioc(IOWR, b'F', 0x2F, size_of::<MarkerData>());
const EVIOCGRAB: u32 = ioc(IOW, b'E', 0x90, size_of::<i32>());

const WF_DU: u32 = 1;
const WF_GC16: u32 = 2;
const WF_GL16: u32 = 3;
const WF_REAGL: u32 = 4;
const PARTIAL: u32 = 0;
const FULL: u32 = 1;

struct Fb {
    file: File,
    mem: *mut u8,
    marker: u32,
}

impl Fb {
    fn open() -> Fb {
        let file = OpenOptions::new().read(true).write(true).open("/dev/fb0").expect("open fb0");
        let len = STRIDE * H;
        // SAFETY: mapping the framebuffer device we just opened, at its documented size.
        let mem = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        assert!(mem != libc::MAP_FAILED, "mmap failed: {}", std::io::Error::last_os_error());
        Fb { file, mem: mem.cast(), marker: 0 }
    }

    fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: the mapping is STRIDE * H bytes and lives as long as self.
        unsafe { std::slice::from_raw_parts_mut(self.mem, STRIDE * H) }
    }

    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, v: u8) {
        let px = self.pixels();
        for row in y..(y + h).min(H) {
            let start = row * STRIDE + x;
            let end = row * STRIDE + (x + w).min(W);
            px[start..end].fill(v);
        }
    }

    /// Sends an update and waits for it to finish; returns (submit, total) times.
    fn update(&mut self, rect: Rect, waveform: u32, mode: u32) -> (Duration, Duration) {
        self.marker += 1;
        let data = UpdateData {
            region: rect,
            waveform_mode: waveform,
            update_mode: mode,
            update_marker: self.marker,
            flags: 0,
            dither_mode: 0,
        };
        let start = Instant::now();
        // SAFETY: the ioctl reads one UpdateData, which outlives the call.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), SEND_UPDATE as _, &data) };
        assert!(ret == 0, "send update: {}", std::io::Error::last_os_error());
        let submitted = start.elapsed();
        let mut wait = MarkerData { update_marker: self.marker, collision_test: 0 };
        // SAFETY: the ioctl reads and writes one MarkerData, which outlives the call.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), WAIT_COMPLETE as _, &mut wait) };
        if ret != 0 {
            eprintln!("wait complete failed: {}", std::io::Error::last_os_error());
        }
        (submitted, start.elapsed())
    }
}

fn whole() -> Rect {
    Rect { top: 0, left: 0, width: W as u32, height: H as u32 }
}

/// Waits for one touch-down and returns its raw (MT_X, MT_Y).
fn wait_touch(touch: &mut File, timeout: Duration) -> Option<(i32, i32)> {
    let deadline = Instant::now() + timeout;
    let (mut x, mut y) = (None, None);
    let mut buf = [0u8; 16 * 64];
    while Instant::now() < deadline {
        let mut fds = libc::pollfd { fd: touch.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut fds, 1, 500) } <= 0 {
            continue;
        }
        let n = touch.read(&mut buf).unwrap_or(0);
        for ev in buf[..n].chunks_exact(16) {
            let kind = u16::from_ne_bytes([ev[8], ev[9]]);
            let code = u16::from_ne_bytes([ev[10], ev[11]]);
            let value = i32::from_ne_bytes([ev[12], ev[13], ev[14], ev[15]]);
            match (kind, code) {
                (3, 53) => x = Some(value),
                (3, 54) => y = Some(value),
                // SYN_MT_REPORT ends a contact's report.
                (0, 2) => {
                    if let (Some(x), Some(y)) = (x, y) {
                        return Some((x, y));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Lets go of any pending touch-up so the next wait sees a fresh touch.
fn drain(touch: &mut File) {
    std::thread::sleep(Duration::from_millis(600));
    let mut buf = [0u8; 16 * 64];
    loop {
        let mut fds = libc::pollfd { fd: touch.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut fds, 1, 0) } <= 0 {
            break;
        }
        let _ = touch.read(&mut buf);
    }
}

fn target(fb: &mut Fb, cx: usize, cy: usize) {
    fb.fill(0, 0, W, H, 0xFF);
    fb.fill(cx - 60, cy - 60, 120, 120, 0x00);
    fb.fill(cx - 40, cy - 40, 80, 80, 0xFF);
    fb.fill(cx - 15, cy - 15, 30, 30, 0x00);
}

fn main() {
    let mut fb = Fb::open();
    let mut touch = File::open("/dev/input/event1").expect("open touch");
    // SAFETY: EVIOCGRAB takes an int by value; the grab ends when the fd closes.
    let grabbed = unsafe { libc::ioctl(touch.as_raw_fd(), EVIOCGRAB as _, 1 as libc::c_int) } == 0;
    println!("touch grabbed: {grabbed}");

    let targets = [(200usize, 300usize), (870, 1200), (870, 300)];
    for (i, (cx, cy)) in targets.into_iter().enumerate() {
        target(&mut fb, cx, cy);
        let (sub, total) = fb.update(whole(), WF_GC16, FULL);
        println!("target {i} at screen ({cx},{cy}): GC16 full submit {sub:?} total {total:?}");
        drain(&mut touch);
        match wait_touch(&mut touch, Duration::from_secs(180)) {
            Some((rx, ry)) => println!("target {i}: raw touch MT_X={rx} MT_Y={ry}"),
            None => println!("target {i}: no touch within 180s"),
        }
    }

    // Timing: page-like content, alternating, for each waveform.
    for (name, wf, mode) in [
        ("GL16 partial", WF_GL16, PARTIAL),
        ("REAGL partial", WF_REAGL, PARTIAL),
        ("DU partial", WF_DU, PARTIAL),
        ("GC16 full", WF_GC16, FULL),
    ] {
        let mut times = Vec::new();
        for round in 0..3 {
            fb.fill(0, 0, W, H, 0xFF);
            for line in 0..40 {
                let y = 80 + line * 32;
                let w = 700 + ((line * 37 + round * 101) % 220);
                fb.fill(60, y, w, 14, 0x20);
            }
            let (_, total) = fb.update(whole(), wf, mode);
            times.push(total.as_millis());
        }
        println!("{name}: full-screen page times ms {times:?}");
    }

    fb.fill(0, 0, W, H, 0xFF);
    fb.update(whole(), WF_GC16, FULL);
    println!("done");
}
