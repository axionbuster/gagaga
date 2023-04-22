//! Thumbnailing

use crate::prim::*;

/// Thumbnail an image file into JPEG with a maximum width and height
/// (while keeping the aspect ratio) and a quality (0-100).
fn ithumbjpg<const W: u32, const H: u32, const Q: u8>(
    file: &[u8],
) -> Result<Vec<u8>> {
    let img = image::load_from_memory(file).map_err(|e| {
        tracing::warn!("Failed to load image from in-memory buffer: {e}");
        std::io::Error::from(std::io::ErrorKind::InvalidData)
    })?;
    let img = img.thumbnail(W, H);
    let fmt = image::ImageOutputFormat::Jpeg(Q);
    let mut cur = std::io::Cursor::new(vec![]);
    img.write_to(&mut cur, fmt)
        .context("while writing image data to in-memory buffer")?;
    Ok(cur.into_inner())
}
