//! Listing API (JSON directory lister)

use axum::response::IntoResponse;
use mime_guess::mime;
use serde_json::json;
use tokio::sync::OnceCell;

use crate::domain::{pathresolve, RealPath};
use crate::primitive::{
    anyhow::Context, tracing::instrument, UnifiedError::*, *,
};
use crate::vfs::{AsyncReadExt, FileStat, PathBuf, VfsV1};
use crate::vfsa::VfsImplA;

/// The root directory of the server.
pub static ROOT: OnceCell<RealPath> = OnceCell::const_new();

/// Use as middleware to resolve "the path" (see [`pathresolve`](crate::domain::pathresolve))
/// from the request. Return 404 if the path fails to resolve.
#[instrument(err, skip(request), fields(path = %userpath.as_ref().map(|x| x.as_str()).unwrap_or("/")))]
pub async fn resolve_path<B>(
    userpath: Option<axum::extract::Path<String>>,
    mut request: axum::http::Request<B>,
) -> Result<axum::http::Request<B>> {
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
    let listing = dirlistjson::<3000>(
        // path (user's control)
        &userpathreal,
        // parent path (server's control)
        ROOT.get().unwrap(),
        // dependency injection
        VfsImplA,
    )
    .await?;

    let json = json!({
        "version": "020",
        "listing": listing,
    });

    let json = json.to_string();

    // Axum: make a response.
    let response = axum::response::Response::builder()
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(json))
        .context("dirlist send make response")?;

    Ok(response.into_response())
}
