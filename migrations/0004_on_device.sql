-- Whether a book in someone's library is to be on their readers. Taking a
-- book off frees room on the device but keeps it in the library, with where
-- they got to in it, which removing it from the library would lose. Both
-- sync streams treat a book that's off as removed, and send it again once
-- it's back on.
ALTER TABLE library_entries ADD COLUMN on_device INTEGER NOT NULL DEFAULT 1
    CHECK (on_device IN (0, 1));
