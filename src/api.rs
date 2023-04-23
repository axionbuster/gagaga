//! All the web-facing API stuff goes here

use std::{fmt::Debug, path::PathBuf};

use async_trait::async_trait;
use axum::{
    body::Body,
    debug_handler,
    extract::State,
    http::{self, header, StatusCode},
    middleware::{from_fn, from_fn_with_state, Next},
    response::IntoResponse,
};
use bytes::BytesMut;
use serde_json::json;
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
            .ok_or_else(|| {
                ApiError::with_status(500)(anyhow!("vpath not set"))
            })
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
    let vpath = match vpath {
        Some(vpath) => vpath.0,
        None => "/".into(),
    };

    let real_path = chroot.join(vpath.clone());

    // Quick check
    if bad_path1(&real_path) {
        return Err((
            StatusCode::BAD_REQUEST,
            anyhow!("bad real path: {:?}", real_path),
        )
            .into());
    }

    // Set
    req.extensions_mut().insert(VPath(vpath));

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
async fn api_thumbnail<const N: usize>(
    Chroot(chroot): Chroot,
    VPath(vpath): VPath,
) -> ApiResult<impl IntoResponse> {
    // Open file, read file, check length
    let real_path = chroot.join(&vpath);
    let mut file = tokio::fs::File::open(&real_path)
        .await
        .context("open file")
        .map_err(ApiError::with_status(404))?;
    let mut buf = BytesMut::with_capacity(N + 1);
    let n = file
        .read_buf(&mut buf)
        .await
        .context("read file")
        .map_err(ApiError::with_status(404))?;
    if n > N {
        return Err((
            StatusCode::BAD_REQUEST,
            anyhow!("file too large ({n} > {N})"),
        )
            .into());
    }

    // Thumbnail, width 16, height 16, quality 30
    let buf = buf.to_vec();
    let jpg = tokio::spawn(async move { ithumbjpg::<16, 16, 30>(&buf) })
        .await
        .context("spawn thumbnailing task")
        .map_err(ApiError::with_status(500))?
        .context("thumbnailing")
        .map_err(ApiError::with_status(404))?;

    // Response
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], jpg))
}

/// Handle listing the directory into a JSON response
#[debug_handler]
#[instrument(err)]
async fn api_list(
    Chroot(chroot): Chroot,
    VPath(vpath): VPath,
) -> ApiResult<impl IntoResponse> {
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
            files.push(md);
            continue;
        } else if md.file_type == FileType::Directory {
            dirs.push(md);
            continue;
        }

        // Follow and then categorize. But, use the ORIGINAL metadata.
        let vpathf = vpath.join(&md.file_name);
        let md2 = follow_get_md(&chroot, &vpathf).await;
        if md2.is_err() {
            continue;
        }
        let md2 = md2.unwrap();
        if md2.file_type == FileType::RegularFile {
            files.push(md2);
            continue;
        } else if md2.file_type == FileType::Directory {
            dirs.push(md2);
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
pub fn build_list_api() -> axum::Router<(), axum::body::Body> {
    use axum::routing::get;

    let root = PathBuf::from("/");

    axum::Router::new()
        .route("/*vpath", get(api_list))
        .route("/", get(api_list))
        .layer(from_fn(mw_guard_virt_path))
        .layer(from_fn(mw_nosniff))
        .layer(from_fn_with_state(root, mw_set_chroot))
}
