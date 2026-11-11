# simple-books

A small self-hosted library for Kobo e-readers. Drop EPUBs on the page, tap sync on
your Kobo, and they're in My Books.

- **Drag and drop.** Title, author and cover come from the file, and books are
  converted to Kobo's faster KEPUB format automatically.
- **Kobo sync.** Uses the Kobo's own sync: new books, edits, deletions and reading
  position. Nothing to install on the device.
- **Private or shared.** Each book is just for you or shared with everyone on the
  server. Others choose which shared books go on their Kobo.
- **No signups.** An admin creates accounts, and each person sets their own password
  through a one-time link.
- **Hardcover.app (optional).** Each person can connect their Hardcover account, and
  what they read on their Kobo shows up there.

It's one Rust binary with SQLite, so there's no database server to run.

## Running it

You need Docker (or Podman) and a reverse proxy with HTTPS. The examples use Caddy.

1. Copy [`compose.yaml`](compose.yaml) to your server and edit the settings at the
   top. At minimum, set `PUBLIC_URL` to the address people will use.
2. Create the data directory, owned by the user in `x-user`, then start it:

   ```sh
   mkdir data
   docker compose up -d
   ```

3. Point your proxy at `127.0.0.1:3000`. [`deploy/Caddyfile.example`](deploy/Caddyfile.example)
   has what Kobo devices need: no compression on `/kobo/*`, an upload size limit, and
   secrets kept out of the access log. It also says which header carries the
   client's address, which differs if Cloudflare is in front of Caddy.
4. On its first start, the server prints a link for making the admin account. Find
   it, open it, and choose your username and password:

   ```sh
   docker compose logs simple-books | grep /setup/
   ```

   It works once, within 72 hours, and only while there are no users. Restarting
   prints a new one.

Admins create everyone else from the Users page, and it makes new links for forgotten
passwords too. If the only admin is locked out, run
`docker compose exec simple-books /app/simple-books admin setup-link <name>`.

## Connecting a Kobo

On the Kobo page, choose "Connect your Kobo" and follow the steps. It changes one line
in your Kobo's settings file, and it does that in your browser: the file is never
uploaded. Then eject the Kobo, wait until your computer says it's safe to unplug it,
and tap sync.

New books appear in My Books after a sync, and download when you first tap them.

Or, on a Kobo Clara BW, install Simple Books' own reader in its place: choose "Install
Simple Books on your Kobo" on the same page, in Chrome or Edge. It brings its own home
screen with no ads, fast page turns and sync. The Kobo's own software stays, and
Simple Books can uninstall itself. See [docs/reader-kobo.md](docs/reader-kobo.md).

## Hardcover

Hardcover is off until the server has a Hardcover client ID. To turn it on:

1. On Hardcover, go to [Developer apps](https://hardcover.app/account/developer-apps/new)
   and create a "Mobile, desktop, or CLI" app with the device code sign-in turned on
   and the scopes `read:me:content`, `read:catalog:search`, `read:library` and
   `write:library`.
2. Put its client ID in `HARDCOVER_CLIENT_ID` in `compose.yaml` and restart.

Each person then connects their own account from the Kobo page. New books become
"Want to read", books you're reading become "Currently reading" with your progress,
and finished books become "Read". It never moves a book backwards on your shelves.
Hardcover's sign-in tokens are stored in the database, so keep backups private.

## Backups

Everything lives in the data directory. Stop the container, copy that directory, and
start it again. The backup includes password hashes and sign-in tokens, so store it
somewhere private.

## Building from source

Needs Rust (see `rust-version` in `Cargo.toml`), the `wasm32-unknown-unknown` target,
[cargo-leptos](https://github.com/leptos-rs/cargo-leptos) and
[kepubify](https://github.com/pgaskin/kepubify).

```sh
cargo leptos build --release
scripts/build-kobo-files.sh   # the Kobo reader, for the installer; needs clang and llvm
scripts/check.sh              # formatting, lints, tests and dependency audits
```

The [`Dockerfile`](Dockerfile) builds the same image as the releases.

## License

[AGPL-3.0-or-later](LICENSE), except for what the Kobo reader brings with it:

- **The English–Swedish dictionary** (`dict-en-sv` on the Kobo) is made from
  [Folkets lexikon](https://folkets-lexikon.csc.kth.se/folkets/), KTH Royal Institute of
  Technology, under [CC BY-SA 2.5](https://creativecommons.org/licenses/by-sa/2.5/). See
  [crates/dictionary/data](crates/dictionary/data/README.md).
- **The fonts**, [Literata](https://github.com/googlefonts/literata) and
  [Intel One Mono](https://github.com/intel/intel-one-mono), are under the SIL Open Font
  License; see [crates/reader-core/fonts](crates/reader-core/fonts).
