-- The simple-books reader syncs with its own protocol, under /device/v1. Its
-- devices are in the same table as Kobos, told apart by kind, so a reader's
-- secret can't be used on the Kobo routes or a Kobo's on the reader's.
ALTER TABLE devices ADD COLUMN kind TEXT NOT NULL DEFAULT 'kobo'
    CHECK (kind IN ('kobo', 'reader'));

-- The reader file's SHA-256, in hex, set with reader_size. A reader checks
-- every download against it before using the file.
ALTER TABLE books ADD COLUMN reader_sha256 TEXT;
-- Files made before this have no hash, so they're made again, in the
-- background, as after a converter upgrade. Until then a reader skips them.
UPDATE books SET reader_revision = NULL;

-- A reader waiting for someone to type its code on the website. The reader
-- holds the secret and only its hash is here; when the code is entered, the
-- hash becomes the new device's and this row goes.
CREATE TABLE device_pairings (
    secret_hash TEXT    PRIMARY KEY,
    code        TEXT    NOT NULL UNIQUE,                 -- without its dash
    name        TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
) STRICT;

-- Where a reader is in each book. Kept apart from kobo_reading_states, since a
-- place in the reader's format and a place in Nickel's can't be turned into
-- each other; both feed Hardcover.
CREATE TABLE reader_places (
    user_id      INTEGER NOT NULL,
    book_id      TEXT    NOT NULL,
    chapter      INTEGER NOT NULL,
    block        INTEGER NOT NULL,
    offset_chars INTEGER NOT NULL,
    -- The reader file's revision the place was taken in. Another revision may
    -- number its blocks differently, so a reader goes by percent instead.
    revision     INTEGER NOT NULL,
    percent      REAL    NOT NULL,
    status       TEXT    NOT NULL CHECK (status IN ('unread', 'reading', 'finished')),
    -- The reader's clock when it was there. The newest place wins.
    read_at      INTEGER NOT NULL,
    device_id    INTEGER REFERENCES devices (id) ON DELETE SET NULL,
    change_seq   INTEGER NOT NULL,
    PRIMARY KEY (user_id, book_id),
    FOREIGN KEY (user_id, book_id) REFERENCES library_entries (user_id, book_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX reader_places_sync ON reader_places (user_id, change_seq);
