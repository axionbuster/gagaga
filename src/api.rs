//! All the web-facing API stuff goes here

use std::{fmt::Debug, path::PathBuf};

use axum::{
    extract::State, http::StatusCode, middleware::Next, response::IntoResponse,
};
use thiserror::Error;

use crate::{
    prim::*,
    vfs::{
        bad_path1, Canonicalize, FileType, ReadMetadata, TokioBacked,
        VirtualPathBuf,
    },
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
    req: axum::http::Request<B>,
    next: Next<B>,
) -> ApiResult<impl IntoResponse> {
    // Quick validation
    if bad_path1(&vpath) {
        return Err(ApiError::BadRequest(anyhow::anyhow!("Bad path").into()));
    }

    // Canonicalize the path
    let can = TokioBacked {
        real_root: state.chroot.clone(),
    };
    let cpath = can.canonicalize(vpath).await.map_err(ApiError::NotFound)?;

    // Strip the prefix to get the virtual path back.
    // It also checks whether the path is inside the chroot.
    let vpath = cpath
        .strip_prefix(&state.chroot)
        .with_context(|| {
            format!(
                "while stripping prefix {:?} from canonical {:?}",
                state.chroot, cpath
            )
        })
        .map_err(Error::IO)
        .map_err(ApiError::NotFound)?;

    // Check the metadata. Beware of TOCTTOU attacks if an internet-
    // connected user may be able to change the filesystem.
    // Here we assume that the filesystem is in full control of the
    // server.
    let meta = can
        .read_metadata(&vpath)
        .await
        .with_context(|| {
            format!("while getting metadata for virtual {:?}", vpath)
        })
        .map_err(Error::IO)
        .map_err(ApiError::NotFound)?;
    if !matches!(meta.file_type, FileType::RegularFile | FileType::Directory) {
        return Err(ApiError::BadRequest(
            anyhow::anyhow!("Bad file type").into(),
        ));
    }

    // In the end, the original virtual path gets admitted.
    Ok(next.run(req).await)
}
