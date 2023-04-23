//! All the web-facing API stuff goes here

use std::{fmt::Debug, path::PathBuf};

use axum::{
    extract::State,
    http::{self, header, StatusCode},
    middleware::Next,
    response::IntoResponse,
};
use bytes::BytesMut;
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::{
    fs::{bad_path1, canonicalize, read_metadata, FileType},
    prim::*,
    thumb::ithumbjpg,
};

/// API Error. This gets converted into Axum error responses
#[derive(Debug, Error)]
enum ApiError {
    #[error("Internal Server Error: {0}")]
    Ise(#[source] Error),

    #[error("Bad Request: {0}")]
    BadRequest(#[source] Error),

    #[error("Not Found: {0}")]
    NotFound(#[source] Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            ApiError::Ise(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
                    .into_response()
            }
            ApiError::BadRequest(_) => {
                (StatusCode::BAD_REQUEST, "Bad Request").into_response()
            }
            ApiError::NotFound(_) => {
                (StatusCode::NOT_FOUND, "Not Found").into_response()
            }
        }
    }
}

/// API Result
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Application State
#[derive(Debug, Clone)]
struct AppState {
    /// The root of the virtual filesystem in terms of an actual path
    chroot: PathBuf,
}

/// Only continue if the path is valid
#[instrument]
async fn mw_guard_virt_path<B: Debug>(
    state: State<AppState>,
    axum::extract::Path(vpath): axum::extract::Path<String>,
    mut req: http::Request<B>,
    next: Next<B>,
) -> ApiResult<impl IntoResponse> {
    // Quick check
    if bad_path1(&vpath) {
        return Err(ApiError::BadRequest(anyhow!("bad path: {}", vpath)));
    }

    // Canonicalize the path
    let cpath = canonicalize(&state.chroot, vpath)
        .await
        .map_err(ApiError::NotFound)?;

    // Strip the prefix to get the virtual path back.
    // It also checks whether the path is inside the chroot.
    let vpath = cpath
        .strip_prefix(&state.chroot)
        .context("strip")
        .map_err(ApiError::NotFound)?;

    // Check the metadata
    let meta = read_metadata(&state.chroot, &vpath)
        .await
        .context("read metadata")
        .map_err(ApiError::NotFound)?;
    if !matches!(meta.file_type, FileType::RegularFile | FileType::Directory) {
        return Err(ApiError::NotFound(anyhow!(
            "not a regular file or directory"
        )));
    }

    // In the end, the original virtual path gets admitted.
    // But, we also push the metadata down there.
    req.extensions_mut().insert(meta);

    Ok(next.run(req).await)
}

/// Thumbnail API
///
/// Thumbnail a file with a maximum tolerance of reading (N) MB.
#[instrument]
async fn api_thumbnail<const N: usize>(
    state: State<AppState>,
    axum::extract::Path(vpath): axum::extract::Path<String>,
) -> ApiResult<impl IntoResponse> {
    // Open file, read file, check length
    let real_path = state.chroot.join(&vpath);
    let mut file = tokio::fs::File::open(&real_path)
        .await
        .context("open file")
        .map_err(ApiError::NotFound)?;
    let mut buf = BytesMut::with_capacity(N + 1);
    let n = file
        .read_buf(&mut buf)
        .await
        .context("read file")
        .map_err(ApiError::NotFound)?;
    if n > N {
        return Err(ApiError::BadRequest(anyhow!(
            "file too large ({n} > {N})"
        )));
    }

    // Thumbnail, width 16, height 16, quality 30
    let buf = buf.to_vec();
    let jpg = tokio::spawn(async move { ithumbjpg::<16, 16, 30>(&buf) })
        .await
        .context("spawn thumbnailing task")
        .map_err(ApiError::Ise)?
        .context("thumbnailing")
        .map_err(ApiError::NotFound)?;

    // Response
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], jpg))
}
