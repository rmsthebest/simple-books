-- Timestamps are integer seconds since the Unix epoch.
-- A book belongs to its uploader. Who has it in their library is a separate
-- fact, in library_entries, and each user's sync stream is built from their own
-- entries. A reading state references an entry, so it can't exist for a book
-- outside the user's library, even if application code is wrong.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT,                                  -- NULL until setup is completed
    is_admin      INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
    disabled_at   INTEGER,
    quota_bytes   INTEGER NOT NULL,
    change_seq    INTEGER NOT NULL DEFAULT 0,            -- per-user sync sequence
    created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE setup_tokens (
    token_hash TEXT    PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE sessions (
    token_hash   TEXT    PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
) STRICT;

CREATE INDEX sessions_user ON sessions (user_id);

CREATE TABLE devices (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name            TEXT    NOT NULL,
    secret_hash     TEXT    NOT NULL UNIQUE,
    -- Set while reconnecting: the older device stays valid until this one is seen.
    replaces_device_id INTEGER REFERENCES devices (id) ON DELETE SET NULL,
    force_full_sync INTEGER NOT NULL DEFAULT 0 CHECK (force_full_sync IN (0, 1)),
    created_at      INTEGER NOT NULL,
    last_seen_at    INTEGER
) STRICT;

CREATE INDEX devices_user ON devices (user_id);
CREATE INDEX devices_replaces ON devices (replaces_device_id);

CREATE TABLE books (
    id               TEXT    PRIMARY KEY,                -- UUID
    owner_id         INTEGER NOT NULL REFERENCES users (id),
    visibility       TEXT    NOT NULL DEFAULT 'private'
                             CHECK (visibility IN ('private', 'shared')),

    title            TEXT    NOT NULL,
    authors_json     TEXT    NOT NULL DEFAULT '[]',
    series           TEXT,
    series_index     REAL,
    description      TEXT,
    publisher        TEXT,
    language         TEXT,
    published_on     TEXT,
    isbn             TEXT,                               -- ISBN-13, for finding it on Hardcover
    cover_id         TEXT,

    original_sha256  TEXT    NOT NULL,
    original_size    INTEGER NOT NULL,

    kepub_state      TEXT    NOT NULL DEFAULT 'pending'
                             CHECK (kepub_state IN ('pending', 'ready', 'failed')),
    kepub_size       INTEGER,
    kepub_attempts   INTEGER NOT NULL DEFAULT 0,
    kepubify_version TEXT,

    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,

    UNIQUE (owner_id, original_sha256)
) STRICT;

CREATE INDEX books_recent ON books (owner_id, created_at DESC);
CREATE INDEX books_cover ON books (cover_id);
CREATE INDEX books_shared ON books (visibility, created_at DESC);

-- One shared copy of any given file.
CREATE UNIQUE INDEX books_shared_file ON books (original_sha256) WHERE visibility = 'shared';

-- Which books are in whose library.
CREATE TABLE library_entries (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    book_id    TEXT    NOT NULL REFERENCES books (id) ON DELETE CASCADE,
    first_seq  INTEGER,                                  -- NULL until sync-visible
    change_seq INTEGER NOT NULL DEFAULT 0,
    added_at   INTEGER NOT NULL,
    PRIMARY KEY (user_id, book_id)
) STRICT;

CREATE INDEX library_entries_sync ON library_entries (user_id, change_seq);
CREATE INDEX library_entries_book ON library_entries (book_id);

-- A separate table, so live-book queries can't forget to filter out removals.
-- first_seq is the earliest the user could have received the book, which decides
-- whether their device may need telling. It's kept across re-adds on purpose.
CREATE TABLE book_tombstones (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    book_id    TEXT    NOT NULL,
    first_seq  INTEGER NOT NULL,
    change_seq INTEGER NOT NULL,
    deleted_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, book_id)
) STRICT;

CREATE INDEX book_tombstones_sync ON book_tombstones (user_id, change_seq);

CREATE TABLE kobo_reading_states (
    user_id       INTEGER NOT NULL,
    book_id       TEXT    NOT NULL,
    status        TEXT    NOT NULL,
    percent       REAL,
    state_json    TEXT    NOT NULL,
    last_modified INTEGER NOT NULL,
    change_seq    INTEGER NOT NULL,
    PRIMARY KEY (user_id, book_id),
    FOREIGN KEY (user_id, book_id) REFERENCES library_entries (user_id, book_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX kobo_reading_states_sync ON kobo_reading_states (user_id, change_seq);

-- Hardcover.app. Each user may connect their own account, so their reading shows up
-- there. The tokens are as secret as a password: never logged, never shown.
CREATE TABLE hardcover_accounts (
    user_id        INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    hardcover_id   INTEGER NOT NULL,
    username       TEXT    NOT NULL,
    access_token   TEXT    NOT NULL,
    access_expires INTEGER NOT NULL,
    -- Rotates on every use, and using one twice ends the connection, so only the
    -- newest is kept.
    refresh_token  TEXT    NOT NULL,
    -- Set when Hardcover stops accepting the connection. The user connects again.
    broken_at      INTEGER,
    connected_at   INTEGER NOT NULL
) STRICT;

-- A sign-in waiting for the user to enter its code on Hardcover.
CREATE TABLE hardcover_sign_ins (
    user_id          INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    device_code      TEXT    NOT NULL,
    user_code        TEXT    NOT NULL,
    verification_url TEXT    NOT NULL,
    poll_seconds     INTEGER NOT NULL,
    next_poll_at     INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    -- Why it ended without connecting, shown once: 'denied', 'expired' or 'failed'.
    outcome          TEXT CHECK (outcome IN ('denied', 'expired', 'failed'))
) STRICT;

-- What a user's Hardcover should say about a book in their library, and what it was
-- last told. Only for users with a connected account. Leaving the library forgets
-- it, and changes nothing on Hardcover.
CREATE TABLE hardcover_books (
    user_id         INTEGER NOT NULL,
    book_id         TEXT    NOT NULL,
    -- 'pending' until looked up; 'missing' if it couldn't be found.
    match           TEXT    NOT NULL DEFAULT 'pending'
                            CHECK (match IN ('pending', 'found', 'missing')),
    -- A link the user gave, used instead of searching.
    chosen_slug     TEXT,
    hc_book_id      INTEGER,
    hc_edition_id   INTEGER,
    hc_slug         TEXT,
    hc_pages        INTEGER,
    -- 1 want to read, 2 currently reading, 3 read, as Hardcover numbers them.
    want_status     INTEGER NOT NULL DEFAULT 1 CHECK (want_status IN (1, 2, 3)),
    want_percent    REAL,
    sent_status     INTEGER,
    sent_pages      INTEGER,
    -- Bumped on every change, so finishing an older one doesn't hide a newer one.
    rev             INTEGER NOT NULL DEFAULT 0,
    due_at          INTEGER,                           -- NULL when nothing is waiting
    attempts        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, book_id),
    FOREIGN KEY (user_id, book_id) REFERENCES library_entries (user_id, book_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX hardcover_books_due ON hardcover_books (due_at) WHERE due_at IS NOT NULL;
