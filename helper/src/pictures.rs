//! Screenshots made smaller and encoded for the trip back.

use base64::Engine as _;
use image::imageops::{self, FilterType};
use image::{ImageEncoder, RgbImage};
use serde_json::{json, Map, Value};
use std::hash::{Hash, Hasher};

/// A number that changes whenever the picture does.
pub fn fingerprint(png: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    png.hash(&mut hasher);
    hasher.finish()
}

/// A number of seconds as a person writes it: 15, 2.5.
pub fn number_text(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn decode(png: &[u8]) -> Result<RgbImage, String> {
    image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map(|picture| picture.to_rgb8())
        .map_err(|error| format!("cannot read the screenshot: {error}"))
}

fn webp(picture: &RgbImage, quality: f32) -> Vec<u8> {
    webp::Encoder::from_rgb(picture.as_raw(), picture.width(), picture.height())
        .encode(quality.clamp(0.0, 100.0))
        .to_vec()
}

fn base64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn resized(picture: &RgbImage, width: u32, height: u32) -> RgbImage {
    imageops::resize(picture, width.max(1), height.max(1), FilterType::Lanczos3)
}

pub struct Look {
    pub webp_base64: String,
    pub screen: [u32; 2],
    pub origin: [i64; 2],
    pub size: [u32; 2],
}

/// A screenshot for `look`: cut to a region, shrunk by `scale`, as WebP.
pub fn look(
    png: &[u8],
    region: Option<[i64; 4]>,
    scale: f64,
    quality: f32,
) -> Result<Look, String> {
    let mut picture = decode(png)?;
    let screen = [picture.width(), picture.height()];
    let mut origin = [0, 0];
    if let Some([x, y, w, h]) = region {
        let (x, y) = (x.max(0) as u32, y.max(0) as u32);
        picture = imageops::crop_imm(&picture, x, y, w.max(1) as u32, h.max(1) as u32).to_image();
        origin = [x as i64, y as i64];
    }
    if scale != 1.0 {
        let width = ((picture.width() as f64 * scale) as u32).max(1);
        let height = ((picture.height() as f64 * scale) as u32).max(1);
        picture = resized(&picture, width, height);
    }
    Ok(Look {
        webp_base64: base64(&webp(&picture, quality)),
        screen,
        origin,
        size: [picture.width(), picture.height()],
    })
}

/// A frame for a preview that draws real images: the screen fitted into the
/// box of `pixel_box` pixels, never larger than the screen or `max_px`.
pub fn frame_image(
    png: &[u8],
    pixel_box: (u32, u32),
    max_px: u32,
) -> Result<Map<String, Value>, String> {
    let mut picture = decode(png)?;
    let screen = [picture.width(), picture.height()];
    let (w, h) = (picture.width() as f64, picture.height() as f64);
    let ratio = (pixel_box.0 as f64 / w).min(pixel_box.1 as f64 / h);
    let display = (((w * ratio) as u32).max(1), ((h * ratio) as u32).max(1));
    // The client scales the picture to the display size, so never send more
    // pixels than the screen has.
    let mut send = ratio.min(1.0);
    if max_px > 0 {
        send = send.min(max_px as f64 / w.max(h));
    }
    if send < 1.0 {
        picture = resized(
            &picture,
            ((w * send) as u32).max(1),
            ((h * send) as u32).max(1),
        );
    }
    let mut result = Map::new();
    result.insert("w".into(), json!(picture.width()));
    result.insert("h".into(), json!(picture.height()));
    result.insert("display_w".into(), json!(display.0));
    result.insert("display_h".into(), json!(display.1));
    result.insert("screen".into(), json!(screen));
    result.insert("webp".into(), json!(base64(&webp(&picture, 70.0))));
    Ok(result)
}

/// A frame for a preview drawn with block characters: the screen scaled to
/// `sx` by `sy` pixels for each of the cells it covers.
pub fn frame_blocks(
    png: &[u8],
    cols: u32,
    rows: u32,
    sx: u32,
    sy: u32,
) -> Result<Map<String, Value>, String> {
    let picture = decode(png)?;
    let (w, h) = (picture.width() as f64, picture.height() as f64);
    // A cell is about twice as tall as it is wide, so the picture keeps its
    // shape when it covers cells_w x cells_h cells with cells_w : 2*cells_h
    // equal to the screen's width : height.
    let ratio = (cols as f64 / w).min(rows as f64 * 2.0 / h);
    let cells_w = ((w * ratio) as u32).max(1);
    let cells_h = ((h * ratio / 2.0) as u32).max(1);
    let size = (cells_w * sx, cells_h * sy);
    let small = resized(&picture, size.0, size.1);
    let mut png_out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png_out,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        small.as_raw(),
        small.width(),
        small.height(),
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|error| format!("cannot encode the frame: {error}"))?;
    let mut result = Map::new();
    result.insert("w".into(), json!(size.0));
    result.insert("h".into(), json!(size.1));
    result.insert("png".into(), json!(base64(&png_out)));
    result.insert("screen".into(), json!([picture.width(), picture.height()]));
    Ok(result)
}
