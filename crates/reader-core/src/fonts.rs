//! The bundled fonts. Fontique's system-font discovery stays off, so nothing
//! looks for fontconfig on the device and the desktop draws the same pages.

use std::sync::Arc;

use parley::{
    FontContext,
    fontique::{Blob, Collection, CollectionOptions, SourceCache},
};

/// Literata, OFL 1.1 (see `fonts/OFL.txt`). Variable in weight and optical size.
static LITERATA: &[u8] = include_bytes!("../fonts/Literata.ttf");
static LITERATA_ITALIC: &[u8] = include_bytes!("../fonts/Literata-Italic.ttf");

pub(crate) const BODY_FAMILY: &str = "Literata";

pub(crate) fn context() -> FontContext {
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });
    for data in [LITERATA, LITERATA_ITALIC] {
        collection.register_fonts(Blob::new(Arc::new(data)), None);
    }
    FontContext {
        collection,
        source_cache: SourceCache::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_bundled_family_is_visible() {
        let mut fonts = context();
        let names: Vec<String> = fonts.collection.family_names().map(str::to_owned).collect();
        assert_eq!(names, [BODY_FAMILY]);
        let family = fonts.collection.family_by_name(BODY_FAMILY).unwrap();
        assert_eq!(family.fonts().len(), 2, "upright and italic");
    }
}
