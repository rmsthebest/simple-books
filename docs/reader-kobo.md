# The simple-books reader on a Kobo Clara BW

Simple Books replaces Nickel's home screen and reader with its own: it starts when the
Kobo is switched on, before Nickel, and takes over the screen. Nickel stays installed and
unchanged, and starts whenever the reader leaves for it. The reader reads books in its
own format (`.book`), and starts on its own home screen with a library list, search and
settings. Paired with the website, it gets its books from the server and keeps your place
in them across readers.

Nothing else needs installing: no KOReader, no Nickelite. The website installs it (see
[Install from the website](#install-from-the-website)), with its own Nickel plugin and
its own SSH server.

## What's installed where

| On the Kobo | From the repo | What it is |
|---|---|---|
| `.adds/simple-books/reader-kobo` | `crates/reader-kobo`, built for `armv7-unknown-linux-musleabihf` | The reader, one static binary |
| `.adds/simple-books/reader.sh` | `kobo/reader.sh` | Stops Nickel, runs the reader, starts Nickel again |
| `.adds/simple-books/simple-books-ssh` | `crates/kobo-ssh` | The SSH server the reader starts |
| `.adds/simple-books/wifi.sh` | `kobo/wifi.sh` | Turns Wi-Fi on and off for the reader |
| `.adds/simple-books/boot.sh` | `kobo/boot.sh` | Runs the reader at boot, before Nickel, once booting into it is on |
| `.adds/simple-books/boot-install.sh` | `kobo/boot-install.sh` | Turns booting into the reader on and off |
| `.adds/simple-books/power.sh` | `kobo/power.sh` | Powers off or restarts, when the reader's power screen asks |
| `.adds/simple-books/dict-en-sv` | `crates/dictionary/data/folkets_en_sv_public.xml.gz`, converted by the build | The English–Swedish dictionary, from [Folkets lexikon](https://folkets-lexikon.csc.kth.se/folkets/) (KTH), CC BY-SA 2.5 |
| `.adds/simple-books/version` | `git describe`, by the build | Which build is installed |
| `/usr/local/Kobo/imageformats/libsimplebooks.so` | `crates/kobo-plugin`, through `.kobo/Kobo.tgz` | The Nickel plugin: starts the helper beside Nickel (see [How it's started](#how-its-started)) |
| `.adds/simple-books/books/*.book` | made by the server, or by hand (below) | Books, on the home screen |

The reader also writes, in `.adds/simple-books/`:

- `library.json`: each book's title, authors, series, when it was last opened and how far
  through it is, read from the books when they're added or changed;
- `cover-*.pic`: each book's cover, decoded and shrunk once for the home screen;
- `position-*.json`: the place in each book;
- `pages-*.json`: each book's page map, so reopening is quick;
- `preferences.json`: the settings, the library's sort, and the light once it's changed
  in the reader;
- `server`: the server to sync with, written by the website's installer;
- `sync-secret`: the key the server gave at pairing. The Kobo's storage is FAT, which
  has no permissions, so anyone who plugs the Kobo in can copy it. Remove the reader on the
  website's device page if the Kobo is lost;
- `sync.json`: what the last sync got, so the next asks only for changes;
- `sync-status.json`: who it's paired with and how the last sync went;
- `books/{id}.book`, and `{id}.part` while downloading: books from the server;
- `reader.log`: what happened, including anything that took over 300 ms, such as opening
  a book, and how each sync ended; cut back once it passes 1 MB;
- `boot`: there while booting into the reader is on;
- `boot-started`: there from when `boot.sh` starts the reader until it reaches home or
  returns;
- `ssh-host-key`: the SSH server's key, made the first time it starts;
- `install-boot`, `start-reader`, `usb-return` and `uninstall`: requests for the helper
  beside Nickel, each removed once it's acted on.

None of these is a format Nickel lists as a book, so its library doesn't show them.

## Install from the website

On the website's **My Kobo** page, in Chrome or Edge on a computer:

1. Plug the Kobo in, and choose **Connect** on it.
2. Under **Install Simple Books on your Kobo**, choose the Kobo's drive, `KOBOeReader`.
   The page checks it's a Kobo with no update waiting, then copies the files, pairs the
   reader with your account and asks for it to start at boot.
3. Eject the Kobo and unplug it. It restarts once to install the plugin, then opens
   Simple Books, paired and ready to sync on Wi-Fi.

Installing again later updates it, keeping its books, place, settings and pairing.

The server serves the files from the folder `KOBO_FILES` names, which the image builds
in. For a server built from source, run `scripts/build-kobo-files.sh` and point
`KOBO_FILES` at `target/kobo-files`, its default.

## Install over SSH

For development, with SSH on from the reader's Sync screen:

```sh
scripts/kobo-deploy.sh 192.168.1.94 book.book
```

That builds everything with `scripts/build-kobo-files.sh`, copies the app folder and any
books given, and puts the plugin straight into `/usr/local/Kobo/imageformats/`, where
Nickel loads it next time it starts (`KOBO_PLUGIN=0` leaves it out). It asks for the
password the Sync screen shows, unless a key of yours is in
`.adds/simple-books/authorized_keys`. Set `KOBO_SSH` to change the ssh command, for
example to add `-o UserKnownHostsFile=...`.

To make a `.book` from an EPUB without the server:

```sh
cargo run --release --features ssr -- convert-book --input book.epub --output book.book
```

## Using it

- **The home screen** shows the book being read, with its cover and how far through it
  is, then the books read lately and the books added lately. Tap any of them to open it
  where it was left. **Library** and **Settings** are at the bottom.
- **The way out** of each screen (**Close**, **Done**, **Back**, **Home** from the library)
  is at the right end of its bottom row, under the thumb of a right hand holding the Kobo.
- **The bar along the top** of every screen but the page, the menu's included, shows the
  day and time (in the time zone set in Nickel), **Wi-Fi** while it's on, and the
  battery, with ↑ while charging. The time moves on each minute.
- **Settings** has text size, line spacing, margins, darkness, contrast, the light and
  its warmth, and the turn guard, each with − and +, and **Justified** and
  **Hyphenated**. A sample paragraph at the top is laid out as a page would be, so the
  change shows at once. The open book is laid out again with them when it's next shown,
  keeping the first line at the top. With the turn guard **Off**, every turn counts,
  even while the panel is still drawing the last.
- **The library** lists every book, a screen at a time, sorted by title, author (surname)
  or series; the choice is kept. **‹** and **›** go through the pages, a tap on a book
  opens it, and **Home** goes back.
- **Search**, from the library, has an on-screen keyboard with å, ä and ö. The books
  whose title, authors or series hold every word typed show as you type, ignoring case
  and accents, so "hjarna" finds "Hjärna". Titles starting with what's typed come first.
- **Turn pages** by tapping: the left third of the page goes back, the right third goes
  forward. Tap a link to follow it. The page number, such as "12 of 340", is under the
  text once the whole book has been counted, a few seconds after it's first opened.
- **Look a word up** by resting a finger on it for half a second. The word is outlined,
  and a panel beside it shows its Swedish translations from Folkets lexikon's
  English–Swedish dictionary: each word class ("verb"), its senses, and an example.
  Inflected words find their base form, so "running" shows "run" too, and a word broken
  at a hyphen is looked up whole. Any tap closes the panel without turning the page. The
  dictionary is [Folkets lexikon](https://folkets-lexikon.csc.kth.se/folkets/), from KTH
  Royal Institute of Technology, under [CC BY-SA
  2.5](https://creativecommons.org/licenses/by-sa/2.5/); the panel credits it too.
- **The menu** opens with a tap in the middle third of the page. It has the front light and
  warmth, text size, darkness and contrast, and along the bottom **Return** after
  following a link, **Home**, **Settings** and **Close**. **Done** in Settings comes back to
  the book. Tap below the menu to close it.
- **Sync**, from Settings, pairs the reader with your library: tap **Pair**, then on the
  website's Kobo page, sign in and type the code shown. The library downloads in the
  background, and you can carry on reading. Then it syncs at start, on waking once half
  an hour has passed, and with **Sync now**: new and changed books come down, removed
  ones go, and where you are in each book goes up. Going to sleep sends where you are,
  if it changed, before the Kobo suspends (for up to a minute). A newer place from another reader is
  used next time you open the book, unless you're reading it. **Unpair**, tapped twice,
  stops syncing and leaves the books on the reader.
- **USB:** plugged into a computer, the reader offers **Connect**, which saves your place
  and leaves for Nickel, saying so while Nickel starts. Nickel then offers to connect, as
  it always does: tap **Connect** there too. Simple
  Books comes back a few seconds after the cable is unplugged. **Not now** goes back to where you
  were, and it doesn't ask again until the cable is plugged in again. A charger doesn't
  count. Plugged in while asleep, it asks on waking.
- **Power:** holding the power button for 3 seconds brings up the power screen, with
  **Power off**, **Restart**, **Go to Nickel** and **Cancel**. The place is saved first.
  Powering off leaves "Switched off" on the screen; hold the power button to switch it
  on again. If nothing is tapped for 15 seconds it goes to Nickel, so holding the button
  is still the way out when taps don't work.
- **Leave** with **Go to Nickel** on the power screen. To come back
  from Nickel, restart the Kobo: hold the power button, choose **Power off**, and switch
  it on again.
- **The Kobo screen**, from Settings, has **Start Simple Books when switched on** and
  **Uninstall Simple Books** (see [Booting into the reader](#booting-into-the-reader) and
  [Remove it](#if-something-goes-wrong)).
- **Sleep** with a short press of the power button, or by leaving it alone for 10
  minutes. The sleep screen, the cover of the book you're reading, comes up at once (a
  book without a cover leaves the page up with "Asleep" along the bottom), and the Kobo
  suspends 15 seconds later,
  once the screen has finished with the panel. Press the power button again to wake. It doesn't sleep while plugged in, since
  that can hang the Clara BW's kernel; it stays on the sleep screen and sleeps once
  unplugged.

## Which server, and Wi-Fi

The reader syncs with the server Nickel syncs with, read from `api_endpoint` in Nickel's
settings as the website's Kobo page set it. Only the address is kept; the token in it
is never read further or logged. A `server` file in `.adds/simple-books/` holding an
address such as `https://books.example.com` is used instead, if it's there.

Wi-Fi is off unless a sync, a pairing or SSH needs it. `wifi.sh` connects to the
networks Nickel has saved, the way Nickel and KOReader do, so set Wi-Fi up in Nickel
first. If it can't connect, the Sync screen says why, and `reader.log` has it too.

## SSH

**SSH** on the reader's Sync screen turns Wi-Fi on, starts an SSH server on port 2222,
and shows how to log in: `ssh -p 2222 root@<address>`, with a password made up fresh
each time. Tap it again to stop it. Sleeping and leaving the reader stop it too, since
the server stops when the reader does.

- **Logging in:** as root, with the password shown, or with a key listed in
  `.adds/simple-books/authorized_keys` (OpenSSH's format, one per line). Anyone who can
  see the screen can log in, so turn it off when you're done.
- **What works:** shells, with or without a terminal, and commands, so `tar | ssh ...`
  and `scp -O` work. There's no SFTP and no port forwarding.
- **The host key** is made the first time and kept in `ssh-host-key`, so it stays the
  same across restarts and installs.
- **The server** is `simple-books-ssh` (`crates/kobo-ssh`), built on
  [russh](https://github.com/Eugeny/russh). It reads the password from the reader and
  stops when the reader closes the pipe, so it can't be left running.

## Booting into the reader

**Start Simple Books when switched on**, on the reader's Kobo screen, makes the reader
start when the Kobo is switched on, before Nickel. The website's installer turns it on.
Turning it off puts things back as they were. Either way the screen says what it did.

- **How it works:** the Kobo's `/etc/rc.local` starts a few services and then Nickel.
  Turning it on adds one line to it, just after the boot animation is
  stopped, which runs `boot.sh`. `boot.sh` runs the reader, and when the reader exits,
  for whatever reason, `rc.local` goes on to start Nickel just as it always does.
  Leaving the reader for Nickel works the same as before. **Power off** and **Restart**
  on the power screen don't start Nickel: the next boot starts the reader again.
- **Safety:**
  - Only an `rc.local` whose SHA-256 is known is changed, and a copy of it is kept in
    `/mnt/aux/simple-books/rc.local.orig`. Taking the line out has to give back exactly
    the original, or nothing is written.
  - `rc.local` runs with `sh -e`, so the line can't fail: if `boot.sh` is missing, it
    does nothing.
  - A firmware update replaces `rc.local`, so after one the Kobo starts Nickel until
    it's turned on again: install from the website again, which asks the plugin to. It
    refuses if the new `rc.local` isn't one it knows.
- **Starting Nickel instead, once:** keep a finger on the screen while the Kobo starts,
  until Nickel appears.
- **If the reader hangs at boot:** hold the power button until the Kobo restarts. The
  next boot is Nickel's, since `boot-started` shows that the last one never reached the
  reader's home screen. The boot after that tries the reader again.
- **Turning it off over USB:** delete `.adds/simple-books/boot`. The line stays in
  `rc.local`, but `boot.sh` then returns at once.
- **The boot counter:** the Kobo offers a factory reset after four boots in a row that
  didn't finish, and normally Nickel tells it each boot finished. When the reader starts
  at boot it does that instead, once its home screen is up, and turns off the white
  light that blinks while the Kobo starts. See
  [`clara-bw-hardware.md`](clara-bw-hardware.md).
- **Wi-Fi at boot:** Nickel hasn't run yet, so the Wi-Fi chip isn't set up. `wifi.sh`
  sets it up the way Nickel does, which adds a few seconds to the first sync.

## How it's started

The reader needs root to change `/etc/rc.local`, and has to come back after Nickel has
run, for USB. Nickel runs as root and loads every library in
`/usr/local/Kobo/imageformats/` (see [`clara-bw-hardware.md`](clara-bw-hardware.md)),
and the Kobo installs files there from `.kobo/Kobo.tgz` when it starts. So:

- **The plugin,** `libsimplebooks.so`, is a 4 KB library with Qt's plugin metadata and
  one constructor. Loaded into Nickel, the constructor starts
  `reader-kobo --in-nickel <Nickel's pid>` as a separate process and returns. It uses no
  Qt and no libc, only a few system calls, so it can't disturb Nickel or depend on the
  firmware's libraries. With a file called `no-plugin` in `.adds/simple-books/` it does
  nothing.
- **The helper,** `reader-kobo --in-nickel`, closes the files it inherited from Nickel
  and leaves for `/`, checks the pid is Nickel's, acts on what's been asked for with a
  file in the app folder, and exits:
  - `uninstall`: turns booting into the reader off and removes the plugin;
  - `install-boot`: turns booting into the reader on;
  - `start-reader`: starts `reader.sh` once Nickel has settled;
  - `usb-return`: waits for the computer to be unplugged, then starts `reader.sh`. It
    waits as a copy of itself in `/tmp` (`--usb-wait`), with nothing open on
    `/mnt/onboard`, not even `reader.log`: to share the drive, Nickel has to unmount it,
    and anything still using it leaves the computer with an empty drive.
- **Who asks:** the website's installer writes `install-boot` and `start-reader`. The
  reader writes `usb-return` when it hands over for USB, and `uninstall` from its Kobo
  screen. What the helper does goes in `reader.log`.

## If something goes wrong

- **It shows an error or nothing happens:** `reader.sh` starts Nickel again whenever the
  reader exits, crash or not. `reader.log` says why it stopped.
- **Taps do nothing:** hold the power button for 3 seconds, let go, and wait 15 seconds.
  The reader leaves for Nickel, saving the place.
- **The screen freezes:** hold the power button until the Kobo restarts. Nickel starts as
  usual, even when booting into the reader is on. This didn't erase anything in tests.
- **Sync fails:** the Sync screen says what went wrong. "Couldn't reach the server" with
  Wi-Fi working may mean the Kobo's clock is wrong, which breaks HTTPS.
- **Nickel misbehaves after an install:** put a file called `no-plugin` in
  `.adds/simple-books/` over USB, and restart. The plugin then does nothing.
- **Remove it:** **Uninstall Simple Books** on the reader's Kobo screen, tapped twice.
  The reader leaves for Nickel, and the helper turns booting into the reader off and
  removes the plugin, so from the next start the Kobo is as it was, but for
  `.adds/simple-books`: delete that over USB to remove the books too. Without the
  reader, deleting `.adds/simple-books` over USB is enough: the line left in `rc.local`
  does nothing without `boot.sh`, and the plugin does nothing without the helper.
