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
   has what Kobo devices need: no compression on `/kobo/*`, an upload size limit, the
   client's address in `X-Real-IP`, and secrets kept out of the access log.
4. Create the first user, an admin, and open the link it prints:

   ```sh
   docker compose exec simple-books /app/simple-books admin create-user yourname --admin
   ```

   Admins create everyone else from the Users page. For a forgotten password, either
   the Users page or `admin setup-link <name>` makes a new link.

## Connecting a Kobo

On the Kobo page, choose "Connect your Kobo" and follow the steps. It changes one line
in your Kobo's settings file, and it does that in your browser: the file is never
uploaded. Then eject the Kobo, wait until your computer says it's safe to unplug it,
and tap sync.

New books appear in My Books after a sync, and download when you first tap them.

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
scripts/check.sh   # formatting, lints, tests and dependency audits
```

The [`Dockerfile`](Dockerfile) builds the same image as the releases.

## License

[AGPL-3.0-or-later](LICENSE).
