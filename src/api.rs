//! All the web-facing API stuff goes here

use axum::http::StatusCode;
use axum::response::IntoResponse;
use thiserror::Error;

use crate::prim::*;

/// API Error. This gets converted into Axum error responses
#[derive(Debug, Error)]
enum ApiError {
    #[error("Internal Server Error: {0}")]
    Ise(
        #[from]
        #[source]
        Error,
    ),

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
