# KOReader on a Kobo Clara BW, firmware 6.0.279225

> **History.** Simple Books doesn't need KOReader or Nickelite: it installs from the
> website and brings its own Nickel plugin and SSH server (see
> [`reader-kobo.md`](reader-kobo.md)). This is how the device was first reached, and
> where `kobo/reader.sh` and `kobo/wifi.sh` were adapted from.

Unpatched KOReader runs on this device but can't exit back to Nickel. Three files
from [idetedi's fork](https://github.com/idetedi/koreader) fix that. Tested on one
physical Clara BW on 2026-11-09.

## Results

| Test | Unpatched nightly | With the fork's files |
|---|---|---|
| Launch from Nickelite | Pass | Pass |
| `Exit > Exit` returns to Nickel | **Fail: stuck in a loop** | **Pass** |
| Battery percentage | Not tested | Pass |
| USB plugged in KOReader, then exit: Connect dialog appears | Not tested | Pass |
| Wi-Fi connects | Not tested | Pass |
| Manual sleep and wake | Not tested | Pass |
| Start KOReader with Wi-Fi on | Not tested | Pass |
| 30 minutes of reading | Not tested | Not tested |
| Automatic sleep | Not tested | Not tested |

Only the exit result has a before and after, so the battery and USB changes aren't
shown to be what made those tests pass.

## Versions

| Part | Version | SHA-256 |
|---|---|---|
| Firmware | 6.0.279225 | |
| Launcher | Nickelite 1.5.0, `Nickelite-1.5.0-firmware-6.0.279225.zip` | `6dd1a19bba1c88fa649726f4f15ddd28c8aeeaab43318e1e04fe74d45fb0bf5e` |
| KOReader | `koreader-kobov5-v2026.07.2-315-gddfc91f_2026-11-06.targz` | `bd2a682820f611d207ab094984aebfd8066ebbdc1ee379fabbdfb4bb99de9049` |
| Fix | `idetedi/koreader` commit `da0cf170cd`, branch `custom` | |

That nightly is the fork's exact merge base, so its three changed files apply cleanly.
They are all interpreted, so nothing needs compiling. NickelMenu doesn't support
firmware 5 and later, which is why this uses Nickelite.

Nickelite is a third-party binary that loads into Nickel, and its repository declares
no licence.

## Install

Back up `.kobo/` first. With the Kobo connected over USB as `$KOBO`:

1. **Launcher.** Check the zip against `SHA256SUMS`, unzip it, and copy `Kobo.tgz` to
   `$KOBO/.kobo/Kobo.tgz`. Its optional `cloud-shortcuts.conf` isn't needed.
2. **KOReader.** Extract the package and copy its `koreader` folder to
   `$KOBO/.adds/koreader`.
3. **The fix.** Copy these three files from the fork. Note that two change location,
   because the package keeps those scripts at its top level:

   | From the fork | To |
   |---|---|
   | `platform/kobo/nickel.sh` | `.adds/koreader/nickel.sh` |
   | `platform/kobo/koreader.sh` | `.adds/koreader/koreader.sh` |
   | `frontend/device/kobo/device.lua` | `.adds/koreader/frontend/device/kobo/device.lua` |

4. **Menu entry.** Create `$KOBO/.adds/nm/koreader` containing:

   ```
   menu_item:main:KOReader:cmd_spawn:quiet:exec /mnt/onboard/.adds/koreader/koreader.sh
   ```

5. **Keep Nickel out of hidden folders.** Add this to the end of
   `$KOBO/.kobo/Kobo/Kobo eReader.conf`, as KOReader's wiki asks:

   ```
   [FeatureSettings]
   ExcludeSyncFolders=(\\.(?!kobo|adobe).+|([^.][^/]*/)+\\..+)
   ```

6. **Eject and unplug.** Wait for the update to finish, then in the **Nickelite** tab open
   **Nickelite Settings → Permissions** and enable **KOReader** and **Commands**.
   KOReader then appears in the Nickelite tab.

## What the fix changes

- **`nickel.sh`:** firmware 6 removed `/etc/init.d/z-nickel-hardware-status`, which the
  unpatched script uses to choose its restart path. The fork also detects the new setup
  through `/etc/rc.local`, and clears leftover daemons, the udev FIFO and `multilog`
  so Nickel can start again.
- **`koreader.sh`:** also removes the relocated `/run/kobo/udev/fifo`, so USB events
  aren't lost.
- **`device.lua`:** falls back to `/sys/class/power_supply/battery` if `bd71827_bat` is
  missing.

## Recovery and removal

- **Frozen device:** hold the power button until it restarts. This didn't erase anything
  here.
- **Remove KOReader:** delete `.adds/koreader` and `.adds/nm/koreader` over USB.
- **Remove Nickelite:** create `.adds/nm/uninstall` and reboot.
- **Don't downgrade the firmware.** Kobo warns that rollbacks can lose content.
- **After a firmware update:** check the `[FeatureSettings]` lines and reinstall the
  launcher, since a Nickelite package only targets one firmware version.
