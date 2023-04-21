//! Process and react to thumbnail service requests.

use axum::response::IntoResponse;
use tokio::sync::OnceCell;

use crate::{
    cachethumb,
    domain::*,
    primitive::{anyhow::Context, tracing::instrument, UnifiedError::*, *},
    vfs::*,
    vfsa::VfsImplA,
};

/// A thumbnail cache, shared between all threads, a channel.
pub static CACHEMPSC: OnceCell<cachethumb::Mpsc> = OnceCell::const_new();

/// Serve SVG File Icon (Font Awesome)
#[instrument]
pub async fn serve_svg_file_icon() -> axum::response::Response {
    let response = axum::response::Response::builder()
        .header("Content-Type", "image/svg+xml")
        .body(axum::body::Body::from(include_str!("file-solid.svg")))
        .context("svg send make response")
        .unwrap();
    response.into_response()
}

/// Serve SVG Folder Icon (Font Awesome)
#[instrument]
pub async fn serve_svg_folder_icon() -> axum::response::Response {
    let response = axum::response::Response::builder()
        .header("Content-Type", "image/svg+xml")
        .body(axum::body::Body::from(include_str!("folder-solid.svg")))
        .context("svg send make response")
        .unwrap();
    response.into_response()
}

/// Serve image loading placeholder PNG file
#[instrument]
pub async fn serve_loading_png() -> &'static [u8] {
    // Font Awesome: image
    include_bytes!("image-solid.png")
}

/// Add static assets caching with public, max-age=(1 hour).
#[instrument]
pub async fn add_static_cache_control(
    mut response: axum::response::Response,
) -> axum::response::Response {
    response.headers_mut().insert(
        "Cache-Control",
        axum::http::HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

/// Serve a specific thumbnail in JPEG format where possible.
/// If the thumbnail is not available, then serve a default thumbnail.
///
/// Preserve aspect ratio while fitting in TWxTH.
#[instrument(err, skip(request))]
pub async fn serve_thumb<B, const TW: u32, const TH: u32>(
    headers: axum::http::HeaderMap,
    request: axum::http::Request<B>,
) -> Result<axum::response::Response> {
    // Get the resolved path & metadata from the request.
    let userpathreal = request.extensions().get::<RealPath>().unwrap().clone();
    let filemetadata = request
        .extensions()
        .get::<Option<FileStat>>()
        .unwrap()
        .clone();

    // If the metadata could not be fetched, then serve the file icon.
    if filemetadata.is_none() {
        return Ok(serve_svg_file_icon().await);
    }
    let filemetadata = filemetadata.unwrap();

    // If directory, then serve the folder icon.
    if filemetadata.isdir() {
        return Ok(serve_svg_folder_icon().await);
    }

    // If not file, reject.
    if !filemetadata.isfile() {
        return Err(NotFound(anyhow::anyhow!(
            "serve_thumb: neither a file nor a directory"
        )));
    }

    // A file. Generate a thumbnail. If successful, serve it.
    // Otherwise, serve the file icon.

    // See if we got a fresh one.
    let cache = CACHEMPSC.get().unwrap();
    let thumb_recall = cache.get(&userpathreal).await;

    // TODO: Refactor. What is this?
    if headers.contains_key(axum::http::header::IF_MODIFIED_SINCE)
        && thumb_recall.is_some()
    {
        // Decide if we should send a 304 Not Modified.
        let clastmod = &thumb_recall.as_ref().unwrap().lastmod;
        let ulastmod = headers.get(axum::http::header::IF_MODIFIED_SINCE);
        if ulastmod.is_some() {
            // Still deciding...
            let ulastmod = ulastmod.unwrap().to_str();
            if ulastmod.is_ok() {
                // Still deciding...
                let ulastmod = ulastmod.unwrap();
                let ulastmod = DateTime::from_rfc2822(ulastmod).unwrap();
                let send = clastmod > &ulastmod;

                if send {
                    let response = axum::response::Response::builder()
                        .status(304)
                        .body(axum::body::Body::empty())
                        .context("thumb send 303 make response")
                        .unwrap();
                    return Ok(response.into_response());
                }
            }
        }
    }

    let thumb = match thumb_recall {
        // Fresh
        Some(thumb) => thumb,
        // Stale or nonexistent
        None => {
            // Make a thumbnail.
            let thumb = match gen_thumb::<TW, TH>(&userpathreal).await {
                Ok(value) => value,
                // Quietly ignore errors in this step.
                Err(e) => {
                    tracing::warn!(
                        "thumb gen error (ignored, sent generic): {e}"
                    );
                    return Ok(serve_svg_file_icon().await);
                }
            };
            // Send the thumbnail to the cache.
            cache.ins(&userpathreal, thumb.clone());
            cachethumb::CacheResponse {
                lastmod: DateTime::now(),
                thumbnail: thumb,
            }
        }
    };

    let response = axum::response::Response::builder()
        .header("Content-Type", "image/jpeg")
        .header("Cache-Control", "public, no-cache")
        .header("Last-Modified", thumb.lastmod.rfc2822())
        .body(axum::body::Body::from(thumb.thumbnail))
        .context("thumb jpeg send make response")?
        .into_response();
    Ok(response)
}

/// Attempt to generate a thumbnail in JPEG format with hard-coded quality.
///
/// Because the generation can take a long time, it is delegated in a blocking
/// thread using Tokio.
#[instrument]
async fn gen_thumb<const TW: u32, const TH: u32>(
    userpathreal: &RealPath,
) -> Result<Vec<u8>> {
    let mut file = VfsImplA.openfile(userpathreal).await?;
    let mut buf = vec![];
    file.read_to_end(&mut buf).await?;
    let cursor = std::io::Cursor::new(buf);
    // Sync block
    let join = tokio::task::spawn_blocking(move || {
        let img = image::io::Reader::new(cursor);
        let img = img.with_guessed_format()?;
        let img = img.decode().context("gen_thumb: cannot decode image")?;
        let img = img.thumbnail(TW, TH);
        let format = image::ImageOutputFormat::Jpeg(50);
        let mut cursor = std::io::Cursor::new(vec![]);
        img.write_to(&mut cursor, format)
            .context("gen_thumb: cannot write image")?;
        Ok(cursor.into_inner())
    });
    join.await.context("gen_thumb: thread join fail")?
}
