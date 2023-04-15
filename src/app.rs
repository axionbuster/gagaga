//! Routing and State

use axum::response::IntoResponse;
use mime_guess::mime;
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;
use tracing::instrument;

use crate::cachethumb;
use crate::domain::RealPath;
use crate::primitive::{anyhow::Context, UnifiedError::*, *};
use crate::vfs::{FileStat, PathBuf, VfsV1};
use crate::vfsa::VfsImplA;

// GLOBAL STATE
// (TODO: Replace with a proper state management system.)

/// The root directory of the server.
pub static ROOT: OnceCell<RealPath> = OnceCell::const_new();

/// A thumbnail cache, shared between all threads, a channel.
pub static CACHEMPSC: OnceCell<cachethumb::Mpsc> = OnceCell::const_new();

// ROUTING

/// Use as middleware to resolve "the path" (see [`pathresolve`](crate::domain::pathresolve))
/// from the request. Return 404 if the path fails to resolve.
#[instrument(err, skip(request), fields(path = %userpath.as_ref().map(|x| x.as_str()).unwrap_or("/")))]
pub async fn resolve_path<B>(
    userpath: Option<axum::extract::Path<String>>,
    mut request: axum::http::Request<B>,
) -> Result<axum::http::Request<B>> {
    // Domain-specific primitives
    use crate::domain::{pathresolve, RealPath};

    let rootdir = ROOT.get().unwrap();

    // If the user didn't provide a path, then serve the root directory.
    let user: PathBuf = if userpath.is_none() {
        PathBuf::from(rootdir.as_ref())
    } else {
        let userpath = userpath.unwrap();
        let userpath = userpath.as_str();
        if userpath == "/" {
            PathBuf::from(rootdir.as_ref())
        } else {
            PathBuf::from(userpath)
        }
    };
    tracing::trace!("Calc path: {user:?}");

    // Resolve the path (convert to absolute, normalize, etc.)
    let realpath: RealPath = pathresolve(&user, rootdir, VfsImplA).await?;
    tracing::trace!("Resolved path: {:?}", realpath);
    request.extensions_mut().insert(realpath.clone());

    // Find the metadata, too, by running fstat (or equivalent).
    let metadata: Option<FileStat> = VfsImplA.stat(&realpath).await.ok();
    tracing::trace!("Metadata: {:?}", metadata);
    request.extensions_mut().insert(metadata);

    Ok(request)
}

/// Serve a file or directory, downloading if a regular file,
/// or listing if a directory.
#[instrument(err, skip(request))]
pub async fn serve_root(
    request: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response> {
    // Domain-specific primitives
    use crate::domain::dirlistjson;

    // Get the resolved path from the request.
    let userpathreal = request.extensions().get::<RealPath>().unwrap().clone();
    let filemetadata = request
        .extensions()
        .get::<Option<FileStat>>()
        .unwrap()
        .clone();

    // If the metadata could not be fetched, then, say, not found.
    if filemetadata.is_none() {
        return Err(NotFound(anyhow::anyhow!(
            "metadata for {:?} could not be fetched",
            userpathreal
        )));
    }
    let filemetadata = filemetadata.unwrap();

    // If it's a regular file, then download it.
    if filemetadata.isfile() {
        // First, let Tokio read it asynchronously.
        let mut file = VfsImplA.openfile(&userpathreal).await?;
        // Read everything into a Vec<u8>.
        let mut buf = vec![];
        file.read_to_end(&mut buf).await?;
        // Guess MIME type by their extension.
        // If can't, say, application/octet-stream.
        let mut mime = mime_guess::from_path(userpathreal.as_ref())
            .first_or_octet_stream();
        // If the file is short enough and it says octet stream,
        // then decide whether it's a text file by deep inspection.
        const ONEMB: usize = 1024 * 1024;
        if buf.len() < ONEMB && mime == mime::APPLICATION_OCTET_STREAM {
            let len = buf.len().min(ONEMB);
            let slice = &buf[..len];
            let is_text = std::str::from_utf8(slice).is_ok();
            if is_text {
                mime = mime::TEXT_PLAIN;
            }
        }
        // If it's HTML, XML, JavaScript, CSS, or JSON, then say it's text/plain.
        // It's because browsers will try to execute them,
        // someone might try to inject malicious code, or
        // someone might try to host a website or part of it on this server.
        if mime == mime::TEXT_HTML
            || mime == mime::TEXT_XML
            || mime == mime::TEXT_JAVASCRIPT
            || mime == mime::APPLICATION_JAVASCRIPT
            || mime == mime::APPLICATION_JAVASCRIPT_UTF_8
            || mime == mime::TEXT_CSS
            || mime == mime::APPLICATION_JSON
        {
            mime = mime::TEXT_PLAIN;
        }
        let mut response_builder = axum::response::Response::builder()
            .header("Content-Type", mime.to_string());
        // if still octet-stream, say it's an attachment.
        // For heavy media such as videos and sounds, also, say it's an attachment.
        // Also, any file greater than or equal to 1MB is an attachment.
        if mime == mime::APPLICATION_OCTET_STREAM
            || mime.type_() == mime::VIDEO
            || mime.type_() == mime::AUDIO
            || buf.len() >= ONEMB
        {
            response_builder =
                response_builder.header("Content-Disposition", "attachment");
        }
        // Attach the body
        let response = response_builder
            .body(axum::body::Body::from(buf))
            .context("file send make response")?;
        let response = response.into_response();

        return Ok(response);
    }

    // If it's not a directory, then say, not found.
    if !filemetadata.isdir() {
        return Err(NotFound(anyhow::anyhow!(
            "serve_user_path: neither a file nor a directory"
        )));
    }

    // A directory. List it with a limit of 3000 files.
    let json = dirlistjson::<3000>(
        // path (user's control)
        &userpathreal,
        // parent path (server's control)
        ROOT.get().unwrap(),
        // dependency injection
        VfsImplA,
    )
    .await?;

    // Axum: make a response.
    let response = axum::response::Response::builder()
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(json))
        .context("dirlist send make response")?;

    Ok(response.into_response())
}

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
                let ulastmod =
                    chrono::DateTime::parse_from_rfc2822(ulastmod).unwrap();
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
                lastmod: chrono::Utc::now(),
                thumbnail: thumb,
            }
        }
    };

    let response = axum::response::Response::builder()
        .header("Content-Type", "image/jpeg")
        .header("Cache-Control", "public, no-cache")
        .header("Last-Modified", thumb.lastmod.to_rfc2822())
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

/// Serve the index page.
#[instrument]
pub async fn serve_index() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("index.html"))
}

/// Serve styles.css.
#[instrument]
pub async fn serve_styles() -> axum::response::Response {
    axum::response::Response::builder()
        .header("Content-Type", "text/css")
        .body(axum::body::Body::from(include_str!("styles.css")))
        .context("serve_styles: make response")
        .unwrap()
        .into_response()
}

/// Serve scripts.js
#[instrument]
pub async fn serve_scripts() -> axum::response::Response {
    axum::response::Response::builder()
        .header("Content-Type", "text/javascript")
        .body(axum::body::Body::from(include_str!("scripts.js")))
        .context("serve_scripts: make response")
        .unwrap()
        .into_response()
}
