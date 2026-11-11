# English–Swedish dictionary data

`folkets_en_sv_public.xml.gz` is the English–Swedish dictionary from
[Folkets lexikon](https://folkets-lexikon.csc.kth.se/folkets/) (KTH), gzipped,
unchanged otherwise. It's what the reader looks a word up in when you hold a finger on it.

- **Source:** <https://folkets-lexikon.csc.kth.se/folkets/folkets_en_sv_public.xml>,
  downloaded 2026-11-11. The file says `version="1.0.4488"`, `last-changed="2024-12-14"`.
- **Licence:** [Creative Commons Attribution-Share Alike 2.5
  Generic](https://creativecommons.org/licenses/by-sa/2.5/), as the file and the site
  state. This licence covers the data and the reader's dictionary file made from it
  (`dict-en-sv`), not the rest of Simple Books.
- **Credit:** Folkets lexikon, KTH Royal Institute of Technology, and everyone who
  contributed words to it.

`scripts/build-kobo-files.sh` turns it into `dict-en-sv` with
`cargo run -p dictionary --features convert --bin build-dictionary`; see
`crates/dictionary`.

To update it, download the XML again, gzip it with `gzip -9 -n`, and update the date and
version above.
