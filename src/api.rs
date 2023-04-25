//! All the web-facing API stuff goes here
//!
//! - Error handling (for the API)
//! - State
//! - Intermediary types
//! - Middleware (e.g., nosniff, http caching)
//! - Endpoints (with routing)

use std::{fmt::Debug, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use axum::{
    body::Body,
    extract::State,
    http::{self, header, HeaderValue, StatusCode},
    middleware::{from_fn, from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::{get, get_service},
};
use bytes::BytesMut;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio_stream::StreamExt;
use tower_http::services::ServeDir;

use crate::{fs::*, prim::*, thumb::*};

/// API Error
///
/// ## Creation examples
///
/// Type 1. Creation from a tuple of a [`StatusCode`] and an [`type@Error`].
///
/// (Any [`anyhow::Error`] can be converted to an [`ApiError`])
///
/// ```rust
/// use crate::api::ApiError;
/// use http::StatusCode;
///
/// let err: ApiError = (StatusCode::NOT_FOUND, "Not Found".into()).into();
/// ```
///
/// Type 2a. Creation using the `with_status` method.
///
/// ```rust
/// use crate::api::ApiError;
///
/// let err = ApiError::with_status(404)("Not Found".into());
/// ```
///
/// Type 2b. Same, but using an explicit name.
///
/// ```rust
/// use crate::api::ApiError;
/// use http::StatusCode;
///
/// let err = ApiError::with_status(StatusCode::NOT_FOUND)("Not Found".into());
/// ```
///
/// Type 3a. Using `map_err` to convert any [`anyhow::Error`] to an [`ApiError`].
///
/// ```rust
/// use crate::api::ApiError;
/// use anyhow::anyhow;
///
/// let wrong = Err(anyhow!("Not Found"));
///
/// let err: ApiError = wrong.map_err(ApiError::with_status(404));
/// ```
///
/// Type 3b. Same, but converting anything that implements [`Into<Error>`]
/// by using [`anyhow::Context`].
///
/// ```rust
/// use crate::api::ApiError;
/// use anyhow::Context;
/// use std::io::Error;
///
/// // Just some kind of non-anyhow error
/// let wrong = Err(Error::from_raw_os_error(2));
///
/// let err: ApiError = wrong
///     .context("Not Found")
///     .map_err(ApiError::with_status(404));
/// ```
///
/// ## Using [`ApiError`] and [`ApiResult`] in [`axum`] endpoints
///
/// Just throw it. It will be converted into plain text (yes, just
/// plain text, not JSON) with the status code you provided.
///
/// The end user will see the canonical error message if one is
/// associated with the status code. If the canonical error message
/// doesn't exist, the user will be greated with an empty response.
///
/// The HTTP status code will be set to the one you provided.
///
/// The headers set by your middleware won't be affected.
#[derive(Debug, Error)]
pub struct ApiError(http::StatusCode, #[source] Error);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApiError {}: {:?}", self.0, self.1)
    }
}

impl ApiError {
    /// Create an error using the status code
    pub fn with_status<E: Into<Error>, S: TryInto<StatusCode> + Copy>(
        status: S,
    ) -> impl Fn(E) -> Self
    where
        <S as TryInto<StatusCode>>::Error: Debug,
    {
        move |e| ApiError(status.try_into().unwrap(), e.into())
    }
}

impl From<(StatusCode, Error)> for ApiError {
    fn from((status, err): (StatusCode, Error)) -> Self {
        ApiError(status, err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = self.0;
        (status, status.canonical_reason().unwrap_or_default()).into_response()
    }
}

/// API Result
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Canonicalize a path and check if it's in the chroot.
///
/// If so, then get the metadata of the object after following all links.
async fn follow_get_md(
    chroot: &RealPath,
    vpath: &VirtualPath,
) -> ApiResult<FileMetadata> {
    // Canonicalize the path
    let cpath = canonicalize(&chroot, vpath)
        .await
        .map_err(ApiError::with_status(404))?;

    // Strip the prefix to get the virtual path back.
    // It also checks whether the path is inside the chroot.
    let vpath = cpath
        .strip_prefix(chroot)
        .context("strip")
        .map_err(ApiError::with_status(404))?;

    // Check the metadata
    let meta = read_metadata(&chroot, &vpath)
        .await
        .map_err(ApiError::with_status(404))?;

    Ok(meta)
}

/// The Chroot type
///
/// This is the directory to serve files from, shared across all
/// services and requests, and set once at startup.
#[derive(Debug, Clone)]
struct Chroot(Arc<PathBuf>);

/// Allow Chroot to be extracted from the request
#[async_trait]
impl axum::extract::FromRequestParts<()> for Chroot {
    type Rejection = ApiError;

    #[instrument]
    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &(),
    ) -> ApiResult<Self> {
        let chroot = parts
            .extensions
            .get::<Chroot>()
            .ok_or_else(|| {
                ApiError::with_status(500)(anyhow!("chroot not set"))
            })
            .map(|chroot| chroot.clone())?;
        Ok(chroot)
    }
}

/// Set the Chroot in the request
#[instrument(skip(req, next))]
async fn mw_set_chroot<B>(
    State(chroot): State<Arc<PathBuf>>,
    mut req: http::Request<B>,
    next: Next<B>,
) -> impl IntoResponse {
    tracing::trace!("mw_set_chroot: {:?}", chroot);
    req.extensions_mut().insert(Chroot(chroot));
    next.run(req).await
}

/// Allow VPath to be extracted from the request
#[async_trait]
impl axum::extract::FromRequestParts<()> for VPath {
    type Rejection = ApiError;

    #[instrument]
    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &(),
    ) -> ApiResult<Self> {
        let vpath = parts
            .extensions
            .get::<VPath>()
            .ok_or_else(|| ApiError::with_status(500)(anyhow!("vpath not set")))
            .map(|vpath| vpath.clone())?;
        Ok(vpath)
    }
}

/// Virtual Path (as an HTTP extension)
#[derive(Debug, Clone)]
struct VPath(Arc<PathBuf>);

/// Only continue if the path is valid.
///
/// Set VPath in the request extensions.
#[instrument(skip(req, next), err)]
async fn mw_guard_virt_path(
    Chroot(chroot): Chroot,
    vpath: Option<axum::extract::Path<PathBuf>>,
    mut req: http::Request<Body>,
    next: Next<Body>,
) -> ApiResult<impl IntoResponse> {
    // Extract PathBuf
    let vpath = vpath.map(|vpath| vpath.0).unwrap_or_default();

    // Quick check
    if bad_path1(&vpath) {
        return Err((
            StatusCode::BAD_REQUEST,
            anyhow!("chk 1/3 bad vpath (quick): {vpath:?}"),
        )
            .into());
    }

    // Strip leading '/', which causes the `join` to silently fail.
    let vpath = vpath.strip_prefix("/").unwrap_or(&vpath);

    // Construct the real path
    let real_path = chroot.join(vpath);
    tracing::trace!("real_path: {real_path:?}");

    // Inclusivity check (follow symlinks)
    let real_path = canonicalize(&*chroot, &vpath)
        .await
        .map_err(ApiError::with_status(404))?;
    if !real_path.starts_with(&*chroot) {
        return Err((
            StatusCode::BAD_REQUEST,
            anyhow!("chk 2/3 bad real path (incl): {real_path:?}"),
        )
            .into());
    }

    // Do another check
    if bad_path1(&real_path) {
        return Err((
            StatusCode::BAD_REQUEST,
            anyhow!("chk 3/3 bad real path (quick 2): {real_path:?}"),
        )
            .into());
    }

    // Set
    req.extensions_mut()
        .insert(VPath(Arc::new(vpath.to_owned())));

    Ok(next.run(req).await)
}

/// No sniff
///
/// Set the `X-Content-Type-Options` header to `nosniff`.
#[instrument(skip(req, next))]
async fn mw_nosniff<B: Debug>(
    req: http::Request<B>,
    next: Next<B>,
) -> impl IntoResponse {
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    res
}

/// Thumbnail API
///
/// Thumbnail a file with a maximum tolerance of reading (N) MB.
#[instrument(err)]
async fn api_thumb<const LIMITMB: usize>(
    Chroot(chroot): Chroot,
    VPath(vpath): VPath,
) -> ApiResult<impl IntoResponse> {
    // Open file, read file, check length
    let real_path = chroot.join(&*vpath);
    let mut file = tokio::fs::File::open(&real_path)
        .await
        .context("open file")
        .map_err(ApiError::with_status(404))?;
    // +1 is to detect over-reading.
    let cap = LIMITMB * 1024 * 1024 + 1;
    let mut buf = BytesMut::new();
    loop {
        let n = file
            .read_buf(&mut buf)
            .await
            .context("read file")
            .map_err(ApiError::with_status(404))?;
        if n == 0 {
            break;
        }
        if buf.len() > cap {
            return Err(ApiError::with_status(404)(anyhow!("file too large")));
        }
    }

    // Make thumbnail. ::<width, height, quality%>
    let jpg = tokio::spawn(async move { ithumbjpg::<16, 16, 50>(&buf) })
        .await
        .context("spawn thumbnailing task")
        .map_err(ApiError::with_status(500))?
        .context("thumbnailing")
        .map_err(ApiError::with_status(404))?;

    // Response
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], jpg))
}

/// HTTP caching for files and directories in general by comparing
/// If-Modified-Since (only). This requires the client to ask the
/// server for revalidation each time the cache is used.
#[instrument(skip(req, next), err)]
async fn mw_cache_http_reval_lmo(
    Chroot(chroot): Chroot,
    VPath(vpath): VPath,
    req: http::Request<Body>,
    next: Next<Body>,
) -> ApiResult<Response> {
    // Read the metadata from the file system and its last modified -> lmo
    let md = read_metadata(&*chroot, &*vpath).await;
    let md = match md {
        Ok(md) => md,
        Err(e) => {
            tracing::warn!("read_metadata: {e:?}");
            return Ok(next.run(req).await);
        }
    };
    let lmo = md.last_modified;
    if lmo.is_none() {
        tracing::trace!("no last modified for virtual path {vpath:?}");
        return Ok(next.run(req).await);
    }
    let lmo = lmo.unwrap();
    tracing::trace!("could read last modified from the file system");
    // NOTE: Once I have the last modified date from the file system,
    // I can send Cache-Control.

    // Get HTTP Last Modified date from the client
    // (If-Modified-Since) -> hmo
    let hmo = req.headers().get(header::IF_MODIFIED_SINCE);
    if let Some(hmo) = hmo {
        tracing::trace!("client sent if-modified-since");
        let hmo = hmo
            .to_str()
            .context("convert if-modified-since to &str")
            .map_err(ApiError::with_status(400))?;
        let hmo = DateTime::from_http(hmo)
            .context("convert &str if-modified-since to DateTime")
            .map_err(ApiError::with_status(400))?;
        // If lmo is earlier than hmo, or equal, then fresh.
        if lmo.seccmp(&hmo).is_le() {
            tracing::trace!("fresh");
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        }
        tracing::trace!("stale");
    } else {
        tracing::trace!("no if-modified-since header from client");
    }
    // Stale or no if-modified-since header
    let mut res = next.run(req).await;
    res.headers_mut().append(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, no-cache"),
    );
    res.headers_mut().append(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&lmo.http())
            .context("convert last modified to &str")
            .map_err(ApiError::with_status(500))?,
    );
    Ok(res.into_response())
}

/// Handle listing the directory into a JSON response
#[instrument(err)]
async fn api_list(
    Chroot(chroot): Chroot,
    VPath(vpath): VPath,
) -> ApiResult<impl IntoResponse> {
    /// Serialize a file's metadata into a JSON object.
    ///
    /// Convert the UNIX timestamp (seconds) into the difference
    /// between the given variable epoch (also UNIX timestamp) and
    /// each file's last modified time, with this equation:
    /// ```
    /// (last modified 2) = (given epoch) - (last modified)
    /// ```
    ///
    /// for each file, a JSON array of four items is returned:
    /// ```
    /// [
    ///     (file name, string),
    ///     (file type, "fi" | "di" | "ln" | string),
    ///     (file size, signed integer | null),
    ///     (last modified 2, signed integer | null),
    /// ]
    /// ```
    ///
    /// Don't be surprised when (last modified 2) is sometimes
    /// negative, though it should be generally positive.
    ///
    /// As of version 0.4.0 of the API (version: "040"), the file type
    /// may be only one of "fi", "di" or "ln". In the future, other
    /// file types may be added.
    fn serfmeta(md: &FileMetadata, epoch: i64) -> Value {
        let name = json!(md.file_name);
        let type_ = match md.file_type {
            FileType::RegularFile => json!("fi"),
            FileType::Directory => json!("di"),
            FileType::Link => json!("ln"),
            // Note: if other variants are later added, I will add
            // code to handle them here.
        };
        let size = json!(md.size);
        let lmos = json!(md.last_modified.map(|s| epoch - s.sgnunixsec()));
        json!([name, type_, size, lmos])
    }

    let mut dirs = vec![];
    let mut files = vec![];

    // Measure the time now and round it down to the second
    let now_sgnunixsec = DateTime::now().sgnunixsec();

    // Read the directory
    let mut stream = list_directory(&*chroot, &*vpath)
        .await
        .context("list directory")
        .map_err(ApiError::with_status(404))?;
    while let Some(md) = stream.next().await {
        if md.is_err() {
            continue;
        }
        let md = md.unwrap();

        // Categorize
        if md.file_type == FileType::RegularFile {
            files.push(serfmeta(&md, now_sgnunixsec));
            continue;
        } else if md.file_type == FileType::Directory {
            dirs.push(serfmeta(&md, now_sgnunixsec));
            continue;
        }

        // Follow and then categorize. But, use the ORIGINAL metadata.
        let vpathf = vpath.join(&md.file_name);
        let md = follow_get_md(&chroot, &vpathf).await;
        if md.is_err() {
            continue;
        }
        let md = md.unwrap();
        if md.file_type == FileType::RegularFile {
            files.push(serfmeta(&md, now_sgnunixsec));
            continue;
        } else if md.file_type == FileType::Directory {
            dirs.push(serfmeta(&md, now_sgnunixsec));
            continue;
        }
        // If neither type even after following, ignore.
    }

    // Append necessary metadata and then serialize
    let value = json!({
        "version": "040",
        "now": now_sgnunixsec,
        "dirs": dirs,
        "files": files,
    })
    .to_string();

    Ok((
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        value,
    ))
}

/// Build a complete router for the list API
#[instrument]
pub fn build_list_api(
    chroot: Arc<PathBuf>,
) -> axum::Router<(), axum::body::Body> {
    axum::Router::new()
        .route("/*vpath", get(api_list))
        .route("/", get(api_list))
        .layer(from_fn(mw_guard_virt_path))
        .layer(from_fn(mw_nosniff))
        .layer(from_fn_with_state(chroot, mw_set_chroot))
}

/// Build a thumbnail server API
#[instrument]
pub fn build_thumb_api(
    chroot: Arc<PathBuf>,
) -> axum::Router<(), axum::body::Body> {
    // Use a limit (10 MB) for reading the file.
    axum::Router::new()
        .route("/*vpath", get(api_thumb::<10>))
        .route("/", get(api_thumb::<10>))
        .layer(from_fn(mw_cache_http_reval_lmo))
        .layer(from_fn(mw_guard_virt_path))
        .layer(from_fn(mw_nosniff))
        .layer(from_fn_with_state(chroot, mw_set_chroot))
}

/// Build a download server API
#[instrument]
pub fn build_download_api(
    chroot: Arc<PathBuf>,
) -> axum::Router<(), axum::body::Body> {
    let servedir =
        ServeDir::new(chroot.as_ref()).append_index_html_on_directories(false);

    axum::Router::new()
        .route("/*vpath", get_service(servedir.clone()))
        .route("/", get_service(servedir))
        .layer(from_fn(mw_guard_virt_path))
        .layer(from_fn(mw_nosniff))
        .layer(from_fn_with_state(chroot, mw_set_chroot))
}
