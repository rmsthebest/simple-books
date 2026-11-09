//! Cover images. Whatever comes in is decoded under limits and encoded again as a
//! plain JPEG, so nothing the uploader crafted reaches a browser or a Kobo: not
//! metadata, not a polyglot file, not a decoder exploit aimed at someone else.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, ImageReader, codecs::jpeg::JpegEncoder};

const MAX_DIMENSION: u32 = 12_000;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const JPEG_QUALITY: u8 = 85;

/// The full cover fits this box: the Clara BW's screen, which is also the largest
/// size a Kobo asked for.
const FULL_WIDTH: u32 = 1072;
const FULL_HEIGHT: u32 = 1448;
/// The thumbnail fits this box: the larger of the two sizes a Kobo's home screen
/// and lists asked for. Anything no wider can be served from it.
pub const THUMB_WIDTH: u32 = 355;
const THUMB_HEIGHT: u32 = 530;

#[derive(Debug, thiserror::Error)]
pub enum CoverError {
    #[error("that isn't a JPEG, PNG or WebP image")]
    Unsupported,
    #[error("that image is damaged or too large")]
    Invalid,
    #[error("couldn't encode the image: {0}")]
    Encode(String),
}

/// The two JPEGs stored for a cover.
#[derive(Debug, Clone)]
pub struct ProcessedCover {
    pub full: Vec<u8>,
    pub thumb: Vec<u8>,
}

/// Turns untrusted image bytes into JPEGs. This is slow, so call it off the async
/// runtime.
pub fn process(bytes: &[u8]) -> Result<ProcessedCover, CoverError> {
    let image = decode(bytes)?;
    Ok(ProcessedCover {
        full: encode(&shrink_to_fit(&image, FULL_WIDTH, FULL_HEIGHT))?,
        thumb: encode(&shrink_to_fit(&image, THUMB_WIDTH, THUMB_HEIGHT))?,
    })
}

fn decode(bytes: &[u8]) -> Result<DynamicImage, CoverError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| CoverError::Invalid)?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP)
    ) {
        return Err(CoverError::Unsupported);
    }

    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    reader.decode().map_err(|_| CoverError::Invalid)
}

/// Never enlarges, which would only add bytes.
fn shrink_to_fit(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    if image.width() <= width && image.height() <= height {
        image.clone()
    } else {
        image.thumbnail(width, height)
    }
}

fn encode(image: &DynamicImage) -> Result<Vec<u8>, CoverError> {
    // Alpha doesn't exist in JPEG; dropping it also strips nothing but pixels.
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode_image(&rgb)
        .map_err(|error| CoverError::Encode(error.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use image::{ImageEncoder, Rgb, RgbImage, codecs::png::PngEncoder};

    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbImage::from_pixel(width, height, Rgb([10, 20, 30]));
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(
                image.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        out
    }

    fn jpeg_size(bytes: &[u8]) -> (u32, u32) {
        let reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .unwrap();
        assert_eq!(reader.format(), Some(ImageFormat::Jpeg));
        reader.into_dimensions().unwrap()
    }

    #[test]
    fn large_images_shrink_to_both_sizes_keeping_their_shape() {
        let cover = process(&png(2000, 3000)).unwrap();
        assert_eq!(jpeg_size(&cover.full), (965, 1448));
        assert_eq!(jpeg_size(&cover.thumb), (353, 530));
    }

    #[test]
    fn small_images_are_not_enlarged() {
        let cover = process(&png(100, 150)).unwrap();
        assert_eq!(jpeg_size(&cover.full), (100, 150));
        assert_eq!(jpeg_size(&cover.thumb), (100, 150));
    }

    #[test]
    fn output_is_always_a_fresh_jpeg() {
        // Bytes after the image end are the classic way to make a polyglot.
        let mut bytes = png(10, 10);
        bytes.extend_from_slice(b"<script>alert(1)</script>");
        let cover = process(&bytes).unwrap();
        let found = cover.full.windows(7).any(|window| window == b"<script");
        assert!(!found);
        assert_eq!(&cover.full[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn other_formats_and_rubbish_are_refused() {
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>";
        assert!(matches!(process(svg), Err(CoverError::Unsupported)));
        assert!(matches!(
            process(b"GIF89a....."),
            Err(CoverError::Unsupported)
        ));
        assert!(matches!(process(b""), Err(CoverError::Unsupported)));
        assert!(matches!(
            process(b"\x89PNG\r\n\x1a\nnot really"),
            Err(CoverError::Invalid)
        ));
    }

    /// A tiny file claiming vast dimensions: the check has to come from the header,
    /// before the pixels are allocated.
    #[test]
    fn a_decompression_bomb_is_refused_from_its_header() {
        let mut bytes = png(1, 1);
        // IHDR's width and height come straight after the signature and chunk header.
        bytes[16..20].copy_from_slice(&50_000u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&50_000u32.to_be_bytes());
        assert!(matches!(process(&bytes), Err(CoverError::Invalid)));
    }
}
