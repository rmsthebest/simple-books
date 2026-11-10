# Kobo Clara BW: what our software can control

Facts gathered on 2026-11-09 from one Clara BW (firmware 6.0.279225), over SSH from
KOReader, without changing anything on the device. They're for building the simple-books
reader. Secrets such as Wi-Fi passwords and tokens were deliberately not read.

## System

| | |
|---|---|
| SoC | MediaTek MT8113 family. The kernel reports "MT8110 board E60V00", with `PLATFORM=mt8113t-ntx` |
| CPU | One Cortex-A53 core running a 32-bit (`armv7l`) userspace, with NEON |
| RAM | 448 MB, plus 512 MB of swap |
| Kernel | 4.9.77, signed and booted by Kobo. Modules can be loaded (`CONFIG_MODULES=y`) |
| C library | glibc 2.35. We build static musl binaries for `armv7-unknown-linux-musleabihf`, linked with `rust-lld`, so this doesn't matter to us |
| Init | SysV: `/etc/inittab`, then `/etc/init.d/rc S` (`/etc/rcS.d`), then runlevel 5 (`/etc/rc5.d`) |
| Python | `/usr/bin/python3` is installed. Useful for probing |

### Kernel features

- **Available:** loop devices, evdev, the USB configfs mass-storage gadget, cgroups,
  seccomp, BPF, `PM_AUTOSLEEP` and wakelocks.
- **Not available:** squashfs, overlayfs, DRM and `/dev/kmem`.

### Partitions

| Partition | Mounted at | Contents |
|---|---|---|
| `mmcblk0p10` | `/` | ext4, read-write. 925 MB, 76% used |
| `mmcblk0p12` | `/mnt/onboard` | FAT, 11.3 GB. What USB shows: books and `.kobo/` |
| `mmcblk0p13` | `/mnt/aux` | ext4, read-write, 973 MB and about 1% used. Kobo keeps Bluetooth config and persisted data here. A good home for our app later, being ext4 rather than FAT |
| `mmcblk0p8` | `/data/init_bin` | ext4, read-only. Only `wf_lut.gz`, the display's waveform tables |

## Boot

**The boot order:**

1. `rcS.d` runs `S10display-init.sh`. It starts `mdpd` (display acceleration), runs
   `pickel init` (which calls `pickel-mtk` on this platform), and starts the boot animation.
2. `S11mount-userdata` checks and mounts `/mnt/onboard` and `/mnt/aux`, and checks the
   boot counter (see the warning below).
3. `S99ota` installs updates:
   - **`.kobo/Kobo.tgz`:** for each file in the archive, it deletes the existing file
     in `/usr/local/Kobo`, then unpacks the archive there. This is how Nickelite gets
     installed. Firmware 6 doesn't use `KoboRoot.tgz`.
   - **`.kobo/update.tar`:** a full firmware update, which reboots into the recovery
     partition.
4. Runlevel 5 ends with `/etc/rc.local`. It starts dbus, pulseaudio, speech-dispatcher,
   `hindenburg`, `memorylogger` and finally
   `/usr/local/Kobo/nickel -platform kobo | multilog t /tmp/nickel-log`.

> **The boot counter will trigger a factory reset.** `mount-userdata` runs
> `bootcount-ctl increment` on every boot. Once `bootcount-ctl boot-ok` fails (the count
> reaches 4, or the checksum is bad), the next boot shows the factory-reset screen and
> waits for a tap. Nickel resets the count after it starts: `libnickel.so` calls
> `bootcount-ctl reset`, and nothing else on the system does.
>
> **So anything that starts instead of Nickel must run `bootcount-ctl reset` once it's
> up.** Otherwise the fourth such boot ends at the factory-reset prompt.
> `bootcount-ctl read` shows the count without changing it.

**Recovery touch region.** `mount-userdata` waits for a tap on the factory-reset screen
inside `1080 250 150 550` for products `spaKoboBW` and its relatives, using
`pickel wait-for-hit`. The arguments look like x, y, width and height in touch
coordinates.

## Display

- **Framebuffer:** `/dev/fb0`, driver `hwtcon`, 1072×1448. Nickel runs it at 32 bits per
  pixel, stride 4288. KOReader sets 8 bits per pixel, `grayscale` 1 and `rotate` 3 with
  `FBIOPUT_VSCREENINFO` (FBInk's `fbdepth -d 8 -R UR`), giving stride 1072, and restores
  Nickel's settings on exit; the simple-books reader does the same. The driver accepts a
  change of depth and ignores it unless `/sys/devices/platform/14000000.hwtcon/mdp_src_format`
  is set to match first: `Y8` for 8 bits, `ABGR32` for 32. Upright portrait is the first
  mode it reports.
- **Updates:** draw into the mmapped framebuffer, then send an update ioctl with magic
  `'F'`. The definitions are in FBInk's `eink/mtk-kobo.h` (GPL-2.0, from MediaTek):

  | ioctl | Number | Argument |
  |---|---|---|
  | `HWTCON_SEND_UPDATE` | `_IOW('F', 0x2E)` | `hwtcon_update_data` |
  | `HWTCON_WAIT_FOR_UPDATE_COMPLETE` | `_IOWR('F', 0x2F)` | `{u32 marker; u32 collision_test}` |
  | `HWTCON_SET_NIGHTMODE` | `_IOW('F', 0x26)` | `i32` |
  | `HWTCON_GET_PANEL_INFO` | `_IOR('F', 0x130)` | panel info |

  `hwtcon_update_data` is:

  ```c
  struct hwtcon_update_data {
      struct { uint32_t top, left, width, height; } update_region;
      uint32_t waveform_mode;   // DU 1, GC16 2, GL16 3, REAGL 4, A2 6, AUTO 257
      uint32_t update_mode;     // 0 partial, 1 full
      uint32_t update_marker;
      uint32_t flags;           // 0x1 dithering, 0x10/0x20/0x40 force A2 output
      int32_t  dither_mode;
  };
  ```

- **`/proc/hwtcon/`** has `cmd`, `temp`, `lut_info`, `fiti` and others. KOReader turns
  night mode on and off by writing `night_mode 4` or `night_mode 0` to `cmd`.
- **Refresh timings**, from the draw test: a static Rust binary that mmaps `/dev/fb0` and
  uses the ioctls above. Each time is for the whole screen, measured from send-update until
  wait-for-complete returns, and was the same over 3 runs:

  | Waveform | Mode | Time |
  |---|---|---|
  | DU | partial | 199 ms |
  | GL16 | partial | 281 ms (333 ms the first time) |
  | REAGL | partial | 293 ms |
  | GC16 | full | 352–412 ms |

  Sending the ioctl takes about 8 ms. The rest is the panel itself. So a clean grey page
  turn can't take less than about 280 ms, and everything else has to fit inside that 8 ms
  of sending: the next page must already be drawn when the tap arrives.

## Touch and buttons

| Device | Name | What it is |
|---|---|---|
| `/dev/input/event0` | `gpio-keys` | One key, code 35 by its capability bitmap. Probably the sleep cover, but not confirmed |
| `/dev/input/event1` | `cyttsp5_mt` | Touch panel |
| `/dev/input/event2` | `bd71828-pwrkey` | Power button. `KEY_POWER` (116) down on press, up on release |

**How the touch panel reports:**

- **Protocol:** it uses multitouch protocol A, where each contact ends with
  `SYN_MT_REPORT`. There's no `ABS_MT_SLOT`. KOReader calls this `touch_snow_protocol`.
- **A touch-down sends:**
  - `BTN_TOOL_FINGER` (325) = 1 and `BTN_TOUCH` (330) = 1;
  - `ABS_MT_TRACKING_ID`, `ABS_MT_X`, `ABS_MT_Y` and `ABS_MT_PRESSURE` = 1;
  - single-touch `ABS_X`, `ABS_Y` and `ABS_PRESSURE` = 1024;
  - then `SYN_MT_REPORT` and `SYN_REPORT`.
- **A lift sends:** `ABS_X`, `ABS_Y`, `ABS_PRESSURE` = 0, the two buttons = 0, and
  `SYN_REPORT`. There's no `SYN_MT_REPORT` on a lift.
- **Axes:** raw `ABS_MT_X` runs 0–1447 and `ABS_MT_Y` runs 0–1071, so the panel reports
  in landscape. In upright portrait, **screen x = 1071 − `ABS_MT_Y`** and **screen y =
  `ABS_MT_X`**. This was measured by tapping targets drawn at (200, 300), (870, 1200) and
  (870, 300), which read (296, 848), (1204, 178) and (298, 217).
- **Recorded behaviour:** 24 taps lasted 67–208 ms. Every tap was exactly one down
  report and one up report, with no repeated downs, so there was no hardware bounce. No
  touch reported any position updates between down and up. Whether swipes report
  movement isn't settled yet.
- **Grabbing:** `EVIOCGRAB` on `event1` keeps the touches from other readers such as
  KOReader or Nickel. The grab ends when the fd closes.

## Power, battery and light

- **Battery:** `/sys/class/power_supply/battery` (`capacity`, `status`). The charger is
  `/sys/class/power_supply/ac` (`online`). Before firmware 6 the battery was called
  `bd71827_bat`.
- **Front light:**
  - `/sys/class/backlight/mxc_msp430.0/brightness` runs 0–100. KOReader uses it for
    brightness.
  - `/sys/class/backlight/lm3630a_led/color` sets the warmth. KOReader maps it to 0–10,
    inverted.
  - `lm3630a_led/brightness` (up to 2047) also exists.
- **Suspend**, as KOReader does it on MediaTek devices:
  1. `echo 1 > /sys/power/state-extended`;
  2. wait about 2 s;
  3. `sync`;
  4. `echo mem > /sys/power/state`;
  5. after waking, `echo 0 > /sys/power/state-extended`.
  - If `mem` fails, write `0` to `state-extended` as well. That's what Nickel does.
  - `standby` isn't available.
  - **Never suspend while plugged in.** On MediaTek this hangs the kernel. Check
    `ac/online` and retry after unplugging.
- **Wi-Fi:** a MediaTek WMT stack.
  - **Modules:** `wmt_drv`, `wmt_chrdev_wifi`, `wmt_cdev_bt` and `wlan_drv_gen4m` are
    loaded once and never unloaded.
  - **Power:** echo `1` to `/dev/wmtWifi` to turn it on and `0` to turn it off. Before
    that, KOReader writes a debug sequence to `/proc/driver/wmt_dbg`: `0xDB9DB9`, `7 9 0`,
    sleep 1 s, `0xDB9DB9`, `7 9 1`.
  - **Connect:** `ifconfig wlan0 up`, then
    `wpa_supplicant -D nl80211 -i wlan0 -c /mnt/onboard/.kobo/wpa_supplicant.conf -C /var/run/wpa_supplicant -B`,
    then `dhcpcd`.
  - KOReader's `enable-wifi.sh` and `disable-wifi.sh` are the reference.
