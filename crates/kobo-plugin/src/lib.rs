//! `libsimplebooks.so`, in `/usr/local/Kobo/imageformats/` on the Kobo.
//!
//! Nickel is a Qt 6 program, and Qt loads every image-format plugin in that
//! folder when it lists the formats it can read, early in Nickel's start.
//! Loading a library runs its constructors, inside Nickel, as root. This
//! one has one constructor, which starts
//! `/mnt/onboard/.adds/simple-books/reader-kobo --in-nickel <Nickel's pid>`
//! as a separate process and returns. It touches nothing else in Nickel:
//! it doesn't use Qt or libc, only a few system calls, so it can't
//! disturb Nickel or depend on its firmware version.
//!
//! For Qt it's a plugin in name only: the metadata note says it handles a
//! format called `simple-books`, and asked for an instance it gives none,
//! which Qt skips. The note is laid out as Qt 6.5's moc lays one out, as
//! `NickelHook` (MIT) plugins such as Nickelite's do.
//!
//! It's installed by `.kobo/Kobo.tgz`, which the Kobo unpacks into
//! `/usr/local/Kobo` on its next start. With `no-plugin` in the app folder
//! it does nothing. See `docs/reader-kobo.md`.
//!
//! On anything but 32-bit ARM it builds with the same metadata and a
//! constructor that does nothing, so the workspace builds and tests
//! everywhere.

#![cfg_attr(target_arch = "arm", no_std)]

const IID: &[u8] = b"org.qt-project.Qt.QImageIOHandlerFactoryInterface";
const CLASS: &[u8] = b"SimpleBooksPlugin";
const KEY: &[u8] = b"simple-books";

/// Qt's plugin metadata: a 4-byte header, then a CBOR map of IID (2),
/// class name (3) and the plugin's own metadata (4), here `{"Keys":
/// ["simple-books"]}`.
const METADATA_LEN: usize =
    4 + 1 + (1 + 2 + IID.len()) + (1 + 1 + CLASS.len()) + (1 + 1 + 5 + 1 + 1 + KEY.len()) + 1;

const fn metadata() -> [u8; METADATA_LEN] {
    // Short CBOR text strings carry their length in the first byte.
    assert!(CLASS.len() < 24 && KEY.len() < 24 && IID.len() < 256);
    let mut out = [0; METADATA_LEN];
    let mut at = 0;
    // Metadata version 1, for Qt 6.5, with the requirements byte libnm.so
    // has.
    let parts: [&[u8]; 12] = [
        &[1, 6, 5, 0x80],
        // An indefinite-length map.
        &[0xbf],
        &[0x02, 0x78, IID.len() as u8],
        IID,
        &[0x03, 0x60 + CLASS.len() as u8],
        CLASS,
        // {"Keys": [KEY]}
        &[0x04, 0xa1],
        &[0x64],
        b"Keys",
        &[0x81, 0x60 + KEY.len() as u8],
        KEY,
        &[0xff],
    ];
    let mut p = 0;
    while p < parts.len() {
        let part = parts[p];
        let mut i = 0;
        while i < part.len() {
            out[at] = part[i];
            at += 1;
            i += 1;
        }
        p += 1;
    }
    assert!(at == METADATA_LEN);
    out
}

/// The note's description, padded to four bytes as ELF notes are.
const DESC_LEN: usize = METADATA_LEN.div_ceil(4) * 4;

/// An ELF note, which Qt finds by its section's name and reads without
/// loading the library.
#[repr(C, align(4))]
struct Note {
    name_size: u32,
    desc_size: u32,
    kind: u32,
    name: [u8; 12],
    desc: [u8; DESC_LEN],
}

const fn note() -> Note {
    let metadata = metadata();
    let mut desc = [0; DESC_LEN];
    let mut i = 0;
    while i < METADATA_LEN {
        desc[i] = metadata[i];
        i += 1;
    }
    Note {
        name_size: 12,
        desc_size: METADATA_LEN as u32,
        // "Qt" and 1: Qt's note type for plugin metadata.
        kind: 0x7451_0001,
        name: *b"qt-project!\0",
        desc,
    }
}

#[used]
#[unsafe(link_section = ".note.qt.metadata")]
static NOTE: Note = note();

/// As Qt declares it: where the metadata is, and how long.
#[repr(C)]
#[derive(Debug)]
pub struct QPluginMetaData {
    data: *const u8,
    size: usize,
}

/// Asked for once the library is loaded: the same metadata as the note.
#[unsafe(no_mangle)]
pub extern "C" fn qt_plugin_query_metadata_v2() -> QPluginMetaData {
    QPluginMetaData {
        data: NOTE.desc.as_ptr(),
        size: METADATA_LEN,
    }
}

/// No instance: Qt finds nothing it can use and moves on.
#[unsafe(no_mangle)]
pub extern "C" fn qt_plugin_instance() -> *mut u8 {
    core::ptr::null_mut()
}

#[used]
#[unsafe(link_section = ".init_array")]
static START: extern "C" fn() = start;

extern "C" fn start() {
    #[cfg(target_arch = "arm")]
    arm::start_helper();
}

/// Writes `n` in decimal into `out`, NUL-terminated.
#[cfg_attr(not(target_arch = "arm"), allow(dead_code))]
fn decimal(mut n: u32, out: &mut [u8; 12]) {
    let mut digits = [0u8; 10];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (n % 10) as u8;
        count += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for i in 0..count {
        out[i] = digits[count - 1 - i];
    }
    out[count] = 0;
}

#[cfg(target_arch = "arm")]
mod arm {
    //! Raw ARM EABI system calls: number in r7, arguments from r0, result
    //! in r0.

    use core::arch::asm;

    use super::decimal;

    /// What the helper is started as.
    const HELPER: &[u8] = b"/mnt/onboard/.adds/simple-books/reader-kobo\0";
    /// With this in the app folder, the plugin does nothing.
    const KILL_SWITCH: &[u8] = b"/mnt/onboard/.adds/simple-books/no-plugin\0";

    const SYS_EXIT_GROUP: usize = 248;
    const SYS_GETPID: usize = 20;
    const SYS_CLONE: usize = 120;
    const SYS_SETSID: usize = 66;
    const SYS_EXECVE: usize = 11;
    const SYS_WAIT4: usize = 114;
    const SYS_ACCESS: usize = 33;
    const SYS_OPEN: usize = 5;
    const SYS_DUP2: usize = 63;
    const SIGCHLD: usize = 17;
    const O_RDWR: usize = 2;

    unsafe fn syscall(number: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize) -> isize {
        let result: isize;
        // SAFETY: the caller passes arguments the call accepts.
        unsafe {
            asm!(
                "svc 0",
                in("r7") number,
                inlateout("r0") arg0 => result,
                in("r1") arg1,
                in("r2") arg2,
                in("r3") arg3,
                options(nostack),
            );
        }
        result
    }

    /// A fork: the child gets 0. Without glibc's fork handlers, so the
    /// child must only make system calls.
    unsafe fn fork() -> isize {
        // SAFETY: clone with only SIGCHLD and no new stack is fork.
        unsafe { syscall(SYS_CLONE, SIGCHLD, 0, 0, 0) }
    }

    fn exit() -> ! {
        loop {
            // SAFETY: ends this process.
            unsafe {
                syscall(SYS_EXIT_GROUP, 0, 0, 0, 0);
            }
        }
    }

    pub(super) fn start_helper() {
        // SAFETY: each call gets NUL-terminated strings and arrays that live
        // until it returns; the children only make system calls.
        unsafe {
            if syscall(SYS_ACCESS, KILL_SWITCH.as_ptr() as usize, 0, 0, 0) == 0 {
                return;
            }
            let nickel = syscall(SYS_GETPID, 0, 0, 0, 0) as u32;
            let child = fork();
            if child != 0 {
                // Nickel: wait for the child, which exits at once.
                if child > 0 {
                    syscall(SYS_WAIT4, child as usize, 0, 0, 0);
                }
                return;
            }
            // The child: its own session, then a grandchild that nothing
            // waits for, so init adopts it.
            syscall(SYS_SETSID, 0, 0, 0, 0);
            if fork() != 0 {
                exit();
            }
            let null = syscall(SYS_OPEN, c"/dev/null".as_ptr() as usize, O_RDWR, 0, 0);
            if null >= 0 {
                for fd in 0..3 {
                    syscall(SYS_DUP2, null as usize, fd, 0, 0);
                }
            }
            let mut pid = [0u8; 12];
            decimal(nickel, &mut pid);
            let argv: [*const u8; 4] = [
                HELPER.as_ptr(),
                c"--in-nickel".as_ptr().cast(),
                pid.as_ptr(),
                core::ptr::null(),
            ];
            let envp: [*const u8; 2] = [
                c"PATH=/sbin:/bin:/usr/sbin:/usr/bin".as_ptr().cast(),
                core::ptr::null(),
            ];
            syscall(
                SYS_EXECVE,
                HELPER.as_ptr() as usize,
                argv.as_ptr() as usize,
                envp.as_ptr() as usize,
                0,
            );
            exit();
        }
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        exit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_metadata_is_laid_out_like_nickelites() {
        let m = metadata();
        assert_eq!(&m[..4], &[1, 6, 5, 0x80]);
        assert_eq!(&m[4..7], &[0xbf, 0x02, 0x78]);
        assert_eq!(usize::from(m[7]), IID.len());
        assert_eq!(&m[8..8 + IID.len()], IID);
        let rest = &m[8 + IID.len()..];
        let mut want = vec![0x03, 0x60 + CLASS.len() as u8];
        want.extend_from_slice(CLASS);
        want.extend_from_slice(&[0x04, 0xa1, 0x64]);
        want.extend_from_slice(b"Keys");
        want.extend_from_slice(&[0x81, 0x60 + KEY.len() as u8]);
        want.extend_from_slice(KEY);
        want.push(0xff);
        assert_eq!(rest, want.as_slice());
    }

    #[test]
    fn the_note_and_the_function_agree() {
        let found = qt_plugin_query_metadata_v2();
        assert_eq!(found.size, METADATA_LEN);
        assert_eq!(NOTE.desc_size as usize, METADATA_LEN);
        assert_eq!(&NOTE.name, b"qt-project!\0");
        assert_eq!(std::mem::align_of::<Note>(), 4);
        assert!(qt_plugin_instance().is_null());
    }

    #[test]
    fn pids_are_written_in_decimal() {
        for (n, want) in [(0, "0"), (7, "7"), (1234, "1234"), (u32::MAX, "4294967295")] {
            let mut out = [0xaa; 12];
            decimal(n, &mut out);
            let end = out.iter().position(|b| *b == 0).unwrap();
            assert_eq!(std::str::from_utf8(&out[..end]).unwrap(), want);
        }
    }
}
