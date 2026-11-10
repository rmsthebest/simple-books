//! The bundled fonts. Fontique's system-font discovery stays off, so nothing
//! looks for fontconfig on the device and the desktop draws the same pages.

use std::sync::Arc;

use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, Size},
};

use parley::{
    FontContext,
    fontique::{Blob, Collection, CollectionOptions, SourceCache},
};

/// Literata, OFL 1.1 (see `fonts/Literata-OFL.txt`). Variable in weight
/// and optical size.
static LITERATA: &[u8] = include_bytes!("../fonts/Literata.ttf");
static LITERATA_ITALIC: &[u8] = include_bytes!("../fonts/Literata-Italic.ttf");
/// Intel One Mono 1.5, OFL 1.1 (see `fonts/IntelOneMono-OFL.txt`), for code.
static MONO: [&[u8]; 4] = [
    include_bytes!("../fonts/IntelOneMono-Regular.ttf"),
    include_bytes!("../fonts/IntelOneMono-Italic.ttf"),
    include_bytes!("../fonts/IntelOneMono-Bold.ttf"),
    include_bytes!("../fonts/IntelOneMono-BoldItalic.ttf"),
];

pub(crate) const BODY_FAMILY: &str = "Literata";
pub(crate) const MONO_FAMILY: &str = "Intel One Mono";
/// Code is set this much smaller than the text around it. A monospace font's
/// letters are wide, so at the same size code looks louder than prose.
pub(crate) const MONO_SCALE: f32 = 0.9;

pub(crate) fn context() -> FontContext {
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });
    for data in [LITERATA, LITERATA_ITALIC].into_iter().chain(MONO) {
        collection.register_fonts(Blob::new(Arc::new(data)), None);
    }
    FontContext {
        collection,
        source_cache: SourceCache::default(),
    }
}

/// A character's advance in a family's first face, at its default
/// variation, per pixel of font size.
pub(crate) fn advance_em(fonts: &mut FontContext, family: &str, c: char) -> Option<f32> {
    let family = fonts.collection.family_by_name(family)?;
    let font = family.fonts().first()?.clone();
    let blob = font.load(Some(&mut fonts.source_cache))?;
    let font = FontRef::from_index(blob.as_ref(), font.index()).ok()?;
    let glyph = font.charmap().map(c)?;
    font.glyph_metrics(Size::new(1.0), LocationRef::default())
        .advance_width(glyph)
}

/// Whether a face's data is one of the Intel One Mono faces.
#[cfg(test)]
pub(crate) fn is_mono(data: &[u8]) -> bool {
    MONO.iter().any(|face| std::ptr::eq(*face, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_bundled_family_is_visible() {
        let mut fonts = context();
        let mut names: Vec<String> = fonts.collection.family_names().map(str::to_owned).collect();
        names.sort();
        assert_eq!(names, [MONO_FAMILY, BODY_FAMILY]);
        let family = fonts.collection.family_by_name(BODY_FAMILY).unwrap();
        assert_eq!(family.fonts().len(), 2, "upright and italic");
        let family = fonts.collection.family_by_name(MONO_FAMILY).unwrap();
        assert_eq!(
            family.fonts().len(),
            4,
            "regular, italic, bold, bold italic"
        );
    }
}
