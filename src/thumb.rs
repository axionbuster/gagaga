//! Thumbnailing

use crate::prim::*;

/// Thumbnail an image file into JPEG with a maximum width and height
/// (while keeping the aspect ratio) and a quality (0-100).
#[instrument]
pub fn ithumbjpg<const W: u32, const H: u32, const Q: u8>(
    file: &[u8],
) -> Result<Vec<u8>> {
    let img = image::load_from_memory(file)
        .context("while loading image from buffer")?;
    let img = img.thumbnail(W, H);
    let fmt = image::ImageOutputFormat::Jpeg(Q);
    let mut cur = std::io::Cursor::new(vec![]);
    img.write_to(&mut cur, fmt)
        .context("while writing image data to in-memory buffer")?;
    Ok(cur.into_inner())
}
