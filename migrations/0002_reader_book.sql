-- Each book also exists in the reader's own format, made from its EPUB in the
-- background. Unlike the KEPUB, it never holds a book back from anyone's sync.
--
-- reader_revision is the converter revision that last finished with the book,
-- whether it worked or not, and NULL if none has. A server with a different
-- revision converts the book again. reader_size is set only while the file exists.
ALTER TABLE books ADD COLUMN reader_revision INTEGER;
ALTER TABLE books ADD COLUMN reader_size INTEGER;
ALTER TABLE books ADD COLUMN reader_attempts INTEGER NOT NULL DEFAULT 0;
