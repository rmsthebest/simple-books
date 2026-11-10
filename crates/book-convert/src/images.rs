//! Images are made gray and no larger than the screen once, here, so the
//! reader only has to place them.

use std::io::Cursor;

use book_format::{ImageInfo, ImageKind, NewImage};
use image::{
    DynamicImage, ExtendedColorType, GrayImage, ImageEncoder, ImageFormat, ImageReader, Luma,
    codecs::{
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType as PngFilter, PngEncoder},
    },
    imageops::FilterType,
};

/// Images this small are decoration, such as a drop cap or a flourish.
const SMALL_SIDE: u32 = 64;
const JPEG_QUALITY: u8 = 85;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageLimits {
    pub(crate) max_side: u32,
    pub(crate) max_alloc: u64,
}

#[derive(Debug)]
pub(crate) enum Prepared {
    Image(NewImage),
    Small,
    Unusable,
}

pub(crate) fn prepare(bytes: &[u8], screen: (u32, u32), limits: ImageLimits) -> Prepared {
    let Ok(mut reader) = ImageReader::new(Cursor::new(bytes)).with_guessed_format() else {
        return Prepared::Unusable;
    };
    let mut decoding = image::Limits::default();
    decoding.max_image_width = Some(limits.max_side);
    decoding.max_image_height = Some(limits.max_side);
    decoding.max_alloc = Some(limits.max_alloc);
    reader.limits(decoding);
    let photo = reader.format() == Some(ImageFormat::Jpeg);
    let Ok(decoded) = reader.decode() else {
        return Prepared::Unusable;
    };
    if decoded.width() <= SMALL_SIDE && decoded.height() <= SMALL_SIDE {
        return Prepared::Small;
    }

    let gray = fit(on_white(&decoded), screen);
    let (width, height) = gray.dimensions();
    let mut out = Vec::new();
    let (kind, written) = if photo {
        let encoder = JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
        (
            ImageKind::Jpeg,
            encoder.write_image(gray.as_raw(), width, height, ExtendedColorType::L8),
        )
    } else {
        let encoder =
            PngEncoder::new_with_quality(&mut out, CompressionType::Best, PngFilter::Adaptive);
        (
            ImageKind::Png,
            encoder.write_image(gray.as_raw(), width, height, ExtendedColorType::L8),
        )
    };
    if written.is_err() {
        return Prepared::Unusable;
    }
    Prepared::Image(NewImage {
        info: ImageInfo {
            width,
            height,
            kind,
        },
        bytes: out,
    })
}

/// Gray, with anything transparent shown against white paper rather than
/// the black that dropping the alpha would give.
fn on_white(image: &DynamicImage) -> GrayImage {
    if !image.color().has_alpha() {
        return image.to_luma8();
    }
    let with_alpha = image.to_luma_alpha8();
    GrayImage::from_fn(image.width(), image.height(), |x, y| {
        let [value, alpha] = with_alpha.get_pixel(x, y).0;
        let (value, alpha) = (u32::from(value), u32::from(alpha));
        Luma([((value * alpha + 255 * (255 - alpha)) / 255) as u8])
    })
}

/// Scales down to fit the screen, never up.
fn fit(image: GrayImage, (max_width, max_height): (u32, u32)) -> GrayImage {
    let (width, height) = image.dimensions();
    if width <= max_width && height <= max_height {
        return image;
    }
    let scale = f64::min(
        f64::from(max_width) / f64::from(width),
        f64::from(max_height) / f64::from(height),
    );
    let new_width = ((f64::from(width) * scale).round() as u32).clamp(1, max_width);
    let new_height = ((f64::from(height) * scale).round() as u32).clamp(1, max_height);
    image::imageops::resize(&image, new_width, new_height, FilterType::CatmullRom)
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::*;

    const LIMITS: ImageLimits = ImageLimits {
        max_side: 10_000,
        max_alloc: 256 * 1024 * 1024,
    };

    fn encoded(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn scales_to_fit_and_keeps_the_kind() {
        let wide = DynamicImage::new_rgb8(2000, 500);
        let Prepared::Image(image) =
            prepare(&encoded(&wide, ImageFormat::Jpeg), (1000, 1000), LIMITS)
        else {
            panic!("not prepared");
        };
        assert_eq!((image.info.width, image.info.height), (1000, 250));
        assert_eq!(image.info.kind, ImageKind::Jpeg);
        let decoded = image::load_from_memory(&image.bytes).unwrap();
        assert_eq!(decoded.color(), image::ColorType::L8);

        let small_enough = DynamicImage::new_rgb8(300, 200);
        let Prepared::Image(image) = prepare(
            &encoded(&small_enough, ImageFormat::Png),
            (1000, 1000),
            LIMITS,
        ) else {
            panic!("not prepared");
        };
        assert_eq!((image.info.width, image.info.height), (300, 200));
        assert_eq!(image.info.kind, ImageKind::Png);
    }

    #[test]
    fn transparency_becomes_paper() {
        let clear = DynamicImage::ImageRgba8(RgbaImage::from_pixel(100, 100, Rgba([0, 0, 0, 0])));
        let Prepared::Image(image) =
            prepare(&encoded(&clear, ImageFormat::Png), (1000, 1000), LIMITS)
        else {
            panic!("not prepared");
        };
        let decoded = image::load_from_memory(&image.bytes).unwrap().to_luma8();
        assert_eq!(decoded.get_pixel(50, 50).0, [255]);
    }

    #[test]
    fn small_broken_and_huge_images() {
        let tiny = DynamicImage::new_rgb8(40, 40);
        assert!(matches!(
            prepare(&encoded(&tiny, ImageFormat::Png), (1000, 1000), LIMITS),
            Prepared::Small
        ));
        assert!(matches!(
            prepare(b"not an image", (1000, 1000), LIMITS),
            Prepared::Unusable
        ));
        let huge = DynamicImage::new_luma8(3000, 10);
        let strict = ImageLimits {
            max_side: 2000,
            ..LIMITS
        };
        assert!(matches!(
            prepare(&encoded(&huge, ImageFormat::Png), (1000, 1000), strict),
            Prepared::Unusable
        ));
    }
}
