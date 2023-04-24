//! All the web-facing API stuff goes here

use std::{fmt::Debug, path::PathBuf};

use async_trait::async_trait;
use axum::{
    body::Body,
    debug_handler,
    extract::State,
    http::{self, header, HeaderValue, StatusCode},
    middleware::{from_fn, from_fn_with_state, Next},
    response::{IntoResponse, Response},
};
use bytes::BytesMut;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio_stream::StreamExt;

use crate::{
    fs::{
        bad_path1, canonicalize, list_directory, read_metadata, FileMetadata,
        FileType, RealPath, VirtualPath,
    },
    prim::*,
    thumb::ithumbjpg,
};

/// API Error
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
#[derive(Debug, Clone)]
struct Chroot(pub PathBuf);

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

/// Set the Chroot from the global state, or return 500.
#[instrument(skip(req, next))]
async fn mw_set_chroot<B>(
    State(chroot): State<PathBuf>,
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
struct VPath(pub PathBuf);

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
    let real_path = canonicalize(&chroot, &vpath)
        .await
        .map_err(ApiError::with_status(404))?;
    if !real_path.starts_with(chroot) {
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
    req.extensions_mut().insert(VPath(vpath.into()));

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
    let real_path = chroot.join(&vpath);
    let mut file = tokio::fs::File::open(&real_path)
        .await
        .context("open file")
        .map_err(ApiError::with_status(404))?;
    // +1 is to check whether it was truncated (sign of an oversized file)
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
    let md = read_metadata(&chroot, &vpath).await;
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
#[debug_handler]
#[instrument(err)]
async fn api_list(
    Chroot(chroot): Chroot,
    VPath(vpath): VPath,
) -> ApiResult<impl IntoResponse> {
    /// Serialize a file's metadata into a JSON object
    fn serfmta(md: &FileMetadata) -> Value {
        let mut value = json!({
            "name": md.file_name,
        });
        let null = json!(null);
        value["type"] = match md.file_type {
            FileType::Directory => json!("dir"),
            FileType::RegularFile => json!("file"),
            FileType::Link => json!("symlink"),
            #[allow(unreachable_patterns)]
            _ => {
                // Want to guard against new variants being added,
                // so log a warning.
                tracing::warn!(
                    "in serfmta, unhandled variant: {ft:?}",
                    ft = md.file_type
                );
                null.clone()
            }
        };
        value["size"] =
            md.size.map_or_else(|| null.clone(), |size| json!(size));
        value["mtime"] = md
            .last_modified
            .map_or_else(|| null.clone(), |t| json!(t.http()));
        value
    }

    let mut dirs = vec![];
    let mut files = vec![];

    // Read the directory
    let mut stream = list_directory(&chroot, &vpath)
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
            files.push(serfmta(&md));
            continue;
        } else if md.file_type == FileType::Directory {
            dirs.push(serfmta(&md));
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
            files.push(serfmta(&md));
            continue;
        } else if md.file_type == FileType::Directory {
            dirs.push(serfmta(&md));
            continue;
        }
        // If neither type even after following, ignore.
    }

    // Append the version and then serialize
    let value = json!({
        "version": "030",
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
pub fn build_list_api(chroot: PathBuf) -> axum::Router<(), axum::body::Body> {
    use axum::routing::get;

    axum::Router::new()
        .route("/*vpath", get(api_list))
        .route("/", get(api_list))
        .layer(from_fn(mw_guard_virt_path))
        .layer(from_fn(mw_nosniff))
        .layer(from_fn_with_state(chroot, mw_set_chroot))
}

/// Build a thumbnail server API
#[instrument]
pub fn build_thumb_api(chroot: PathBuf) -> axum::Router<(), axum::body::Body> {
    use axum::routing::get;

    // Use a limit (10 MB) for reading the file.
    axum::Router::new()
        .route("/*vpath", get(api_thumb::<10>))
        .route("/", get(api_thumb::<10>))
        .layer(from_fn(mw_cache_http_reval_lmo))
        .layer(from_fn(mw_guard_virt_path))
        .layer(from_fn(mw_nosniff))
        .layer(from_fn_with_state(chroot, mw_set_chroot))
}
