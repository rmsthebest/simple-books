# The simple-books reader on a Kobo Clara BW

The reader runs as an app launched from Nickelite, the way KOReader does: it stops
Nickel, takes over the screen, and starts Nickel again when you leave it. Nickel stays
installed and untouched. It reads books in the reader's own format (`.book`), and
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
| `.adds/nm/simple-books` | `kobo/nickelite-menu` | The **Simple Books**, **SSH on** and **SSH off** entries in the Nickelite tab |
| `.adds/simple-books/books/*.book` | made by the server, or by hand (below) | Books, on the home screen |

The reader also writes, in `.adds/simple-books/`:

- `library.json`: each book's title, authors, series, when it was last opened and how far
  through it is, read from the books when they're added or changed;
- `cover-*.pic`: each book's cover, decoded and shrunk once for the home screen;
- `position-*.json`: the place in each book;
- `pages-*.json`: each book's page map, so reopening is quick;
- `preferences.json`: the settings, the library's sort, and the light once it's changed
  in the reader;
- `sync-secret`, readable only by root: the key the server gave at pairing;
- `sync.json`: what the last sync got, so the next asks only for changes;
- `sync-status.json`: who it's paired with and how the last sync went;
- `books/{id}.book`, and `{id}.part` while downloading: books from the server;
- `reader.log`: what happened, including anything that took over 300 ms, such as opening
  a book; cut back once it passes 1 MB.

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

## If something goes wrong

- **It shows an error or nothing happens:** `reader.sh` starts Nickel again whenever the
  reader exits, crash or not. `reader.log` says why it stopped.
- **Taps do nothing:** hold the power button for 3 seconds and let go. The reader leaves
  for Nickel, saving the place.
- **The screen freezes:** hold the power button until the Kobo restarts. Nickel starts as
  usual. This didn't erase anything in the KOReader tests.
- **Sync fails:** the Sync screen says what went wrong. "Couldn't reach the server" with
  Wi-Fi working may mean the Kobo's clock is wrong, which breaks HTTPS.
- **Remove it:** delete `.adds/simple-books` and `.adds/nm/simple-books` over USB.
