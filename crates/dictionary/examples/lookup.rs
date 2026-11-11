//! Looks words up in a dictionary file, as the reader would:
//!
//!     cargo run -p dictionary --example lookup -- target/dict-en-sv running ran "dog's"

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: lookup <dictionary file> <word>...");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("the dictionary file");
    let started = std::time::Instant::now();
    let dictionary = dictionary::Dictionary::parse(text).expect("a dictionary");
    eprintln!("{dictionary:?} read in {:?}", started.elapsed());
    for word in args {
        println!("{word}:");
        for entry in dictionary.lookup(&word) {
            println!("  {} ({})", entry.word, entry.class);
            for sense in entry.senses.iter().take(4) {
                println!(
                    "    {} | {} | {:?}",
                    sense.translations, sense.note, sense.example
                );
            }
        }
    }
}
