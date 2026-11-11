//! Makes the reader's dictionary file from Folkets lexikon's XML, gzipped
//! or not:
//!
//!     cargo run -p dictionary --features convert --bin build-dictionary -- \
//!         crates/dictionary/data/folkets_en_sv_public.xml.gz target/dict-en-sv

use std::{fs, io::Read, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: build-dictionary <folkets_en_sv_public.xml[.gz]> <output>");
        return ExitCode::FAILURE;
    };
    match build(input, output) {
        Ok(entries) => {
            eprintln!("{output}: {entries} entries");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("build-dictionary: {error}");
            ExitCode::FAILURE
        }
    }
}

fn build(input: &str, output: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let bytes = fs::read(input)?;
    let mut xml = String::new();
    let gzipped = std::path::Path::new(input)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"));
    if gzipped {
        flate2::read::GzDecoder::new(bytes.as_slice()).read_to_string(&mut xml)?;
    } else {
        xml = String::from_utf8(bytes)?;
    }
    let file = dictionary::convert::convert(&xml)?;
    let entries = dictionary::Dictionary::parse(file.clone())?.len();
    fs::write(output, file)?;
    Ok(entries)
}
