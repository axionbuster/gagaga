//! All the web-facing API stuff goes here

use std::{fmt::Debug, path::PathBuf};

use axum::{
    extract::State,
    http::{self, header, StatusCode},
    middleware::Next,
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

/// API Result
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Application State
#[derive(Debug, Clone)]
struct AppState {
    /// The root of the virtual filesystem in terms of an actual path
    chroot: PathBuf,
}

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

/// Only continue if the path is valid
#[instrument]
async fn mw_guard_virt_path<B: Debug>(
    state: State<AppState>,
    axum::extract::Path(vpath): axum::extract::Path<std::path::PathBuf>,
    mut req: http::Request<B>,
    next: Next<B>,
) -> ApiResult<impl IntoResponse> {
    // Quick check
    if bad_path1(&vpath) {
        return Err((
            StatusCode::BAD_REQUEST,
            anyhow!("bad path: {:?}", vpath),
        )
            .into());
    }

    // Follow (canonicalize and get the metadata thereof) the link
    // if it's a link; otherwise, just get the metadata.
    let meta = follow_get_md(&state.chroot, &vpath).await?;

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
#[instrument]
async fn api_list(
    State(state): State<AppState>,
    axum::extract::Path(vpath): axum::extract::Path<std::path::PathBuf>,
) -> ApiResult<impl IntoResponse> {
    let mut dirs = vec![];
    let mut files = vec![];

    // Read the directory
    let mut stream = list_directory(&state.chroot, &vpath)
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
        let md2 = follow_get_md(&state.chroot, &vpathf).await;
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
