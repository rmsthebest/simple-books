# The simple-books reader on a Kobo Clara BW

The reader runs as an app launched from Nickelite, the way KOReader does: it stops
Nickel, takes over the screen, and starts Nickel again when you leave it. It can also
start when the Kobo is switched on, before Nickel (see [Booting into the
reader](#booting-into-the-reader)). Nickel stays installed and untouched. It reads books in the reader's own format (`.book`), and
starts on its own home screen with a library list, search and settings. Paired with the
website, it gets its books from the server and keeps your place in them across readers.

It needs Nickelite installed with its **Commands** permission on, as set up for
KOReader in [`koreader-clara-bw.md`](koreader-clara-bw.md).

## What's installed where

| On the Kobo | From the repo | What it is |
|---|---|---|
| `.adds/simple-books/reader-kobo` | `crates/reader-kobo`, built for `armv7-unknown-linux-musleabihf` | The reader, one static binary |
| `.adds/simple-books/reader.sh` | `kobo/reader.sh` | Stops Nickel, runs the reader, starts Nickel again |
| `.adds/simple-books/ssh.sh` | `kobo/ssh.sh` | Starts and stops an SSH server, from Nickel or the reader |
| `.adds/simple-books/wifi.sh` | `kobo/wifi.sh` | Turns Wi-Fi on and off for the reader |
| `.adds/simple-books/boot.sh` | `kobo/boot.sh` | Runs the reader at boot, before Nickel, once booting into it is on |
| `.adds/simple-books/boot-install.sh` | `kobo/boot-install.sh` | Turns booting into the reader on and off |
| `.adds/nm/simple-books` | `kobo/nickelite-menu` | The **Simple Books**, **SSH on**, **SSH off**, **Boot into Simple Books** and **Boot into Nickel** entries in the Nickelite tab |
| `.adds/simple-books/books/*.book` | made by the server, or by hand (below) | Books, on the home screen |

The reader also writes, in `.adds/simple-books/`:

- `library.json`: each book's title, authors, series, when it was last opened and how far
  through it is, read from the books when they're added or changed;
- `cover-*.pic`: each book's cover, decoded and shrunk once for the home screen;
- `position-*.json`: the place in each book;
- `pages-*.json`: each book's page map, so reopening is quick;
- `preferences.json`: the settings, the library's sort, and the light once it's changed
  in the reader;
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
  returns.

Nickel doesn't list anything under `.adds`, given the `ExcludeSyncFolders` line from the
KOReader setup.

## Install over Wi-Fi

With KOReader open and its SSH server on:

```sh
scripts/kobo-deploy.sh 192.168.1.94 book.book
```

That builds the reader, then copies the app, the launch script, the menu entry and any
books given. Set `KOBO_SSH` to change the ssh command, for example to add `-o
UserKnownHostsFile=...`. Then leave KOReader for Nickel, and **Simple Books** appears in
the Nickelite tab. If it doesn't, restart the Kobo once.

To make a `.book` from an EPUB without the server:

```sh
cargo run --release --features ssr -- convert-book --input book.epub --output book.book
```

## Using it

- **The home screen** shows the book being read, with its cover and how far through it
  is, then the books read lately and the books added lately. Tap any of them to open it
  where it was left. **Library**, **Settings** and **Exit to Nickel** are at the bottom.
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
- **Turn pages** by tapping: the left third of the page goes back, anywhere else goes
  forward. Tap a link to follow it.
- **The menu** opens with a tap along the top of the page. It has the front light and
  warmth, text size, darkness and contrast, **Return** after following a link, **Home**,
  **Close** and **Exit to Nickel**. Tap below it to close it.
- **Sync**, from Settings, pairs the reader with your library: tap **Pair**, then on the
  website's Kobo page, sign in and type the code shown. The library downloads in the
  background, and you can carry on reading. Then it syncs at start, on waking once half
  an hour has passed, and with **Sync now**: new and changed books come down, removed
  ones go, and where you are in each book goes up. A newer place from another reader is
  used next time you open the book, unless you're reading it. **Unpair**, tapped twice,
  stops syncing and leaves the books on the reader.
- **USB:** plugged into a computer, the reader offers **Connect**, which saves your place
  and leaves for Nickel. Nickel then offers to connect, as it always does, and **Simple
  Books** in the Nickelite tab comes back afterwards. **Not now** goes back to where you
  were, and it doesn't ask again until the cable is plugged in again. A charger doesn't
  count. Plugged in while asleep, it asks on waking.
- **Leave** with **Exit to Nickel** in the menu, or by holding the power button for 3
  seconds.
- **Sleep** with a short press of the power button, or by leaving it alone for 10
  minutes. The sleep screen comes up at once, and the Kobo suspends 15 seconds later,
  once the screen has finished with the panel. Press the power button again to wake. It doesn't sleep while plugged in, since
  that can hang the Clara BW's kernel; it stays on the sleep screen and sleeps once
  unplugged.

## Which server, and Wi-Fi

The reader syncs with the server Nickel syncs with, read from `api_endpoint` in Nickel's
settings as the website's Kobo page set it. Only the address is kept; the token in it
is never read further or logged. A `server.txt` in `.adds/simple-books/` holding an
address such as `https://books.example.com` is used instead, if it's there.

Wi-Fi is off unless a sync, a pairing or SSH needs it. `wifi.sh` connects to the
networks Nickel has saved, the way Nickel and KOReader do, so set Wi-Fi up in Nickel
first. If it can't connect, the Sync screen says why, and `reader.log` has it too.

## SSH

**SSH** on the reader's Sync screen turns Wi-Fi on and starts the same server as **SSH
on** below, and shows the address to log in at. Tap it again to stop it. Sleeping and
leaving the reader stop it too.

### From Nickel

**SSH on** in the Nickelite tab starts an SSH server on port 2222 while Nickel runs, so
there's no need to open KOReader to reach the Kobo. Wi-Fi has to be on in Nickel.

- It's KOReader's own server, started the way KOReader's SSH plugin starts it, so it has
  the same host key, and KOReader's SSH menu shows it running.
- It lets anyone on the network log in as root without a password, like KOReader's
  **Login without password** option. Leave it off when you're not using it.
- **SSH off** stops it. Starting the reader stops it too, since the reader turns Wi-Fi
  off, and so does restarting the Kobo.

## Booting into the reader

**Boot into Simple Books** in the Nickelite tab makes the reader start when the Kobo is
switched on, before Nickel. **Boot into Nickel** puts things back as they were. Each one
shows a message saying what it did.

- **How it works:** the Kobo's `/etc/rc.local` starts a few services and then Nickel.
  **Boot into Simple Books** adds one line to it, just after the boot animation is
  stopped, which runs `boot.sh`. `boot.sh` runs the reader, and when the reader exits,
  for whatever reason, `rc.local` goes on to start Nickel just as it always does.
  Leaving the reader for Nickel, from its menu or by holding the power button, works
  the same as before.
- **Safety:**
  - Only an `rc.local` whose SHA-256 is known is changed, and a copy of it is kept in
    `/mnt/aux/simple-books/rc.local.orig`. Taking the line out has to give back exactly
    the original, or nothing is written.
  - `rc.local` runs with `sh -e`, so the line can't fail: if `boot.sh` is missing, it
    does nothing.
  - A firmware update replaces `rc.local`, so after one the Kobo starts Nickel until
    **Boot into Simple Books** is used again. It refuses if the new `rc.local` isn't one
    it knows.
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

## If something goes wrong

- **It shows an error or nothing happens:** `reader.sh` starts Nickel again whenever the
  reader exits, crash or not. `reader.log` says why it stopped.
- **Taps do nothing:** hold the power button for 3 seconds and let go. The reader leaves
  for Nickel, saving the place.
- **The screen freezes:** hold the power button until the Kobo restarts. Nickel starts as
  usual, even when booting into the reader is on. This didn't erase anything in the
  KOReader tests.
- **Sync fails:** the Sync screen says what went wrong. "Couldn't reach the server" with
  Wi-Fi working may mean the Kobo's clock is wrong, which breaks HTTPS.
- **Remove it:** use **Boot into Nickel** first, if booting into the reader is on, then
  delete `.adds/simple-books` and `.adds/nm/simple-books` over USB. Deleting them
  without that is safe too, since the line left in `rc.local` does nothing without
  `boot.sh`.
