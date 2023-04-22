//! All the web-facing API stuff goes here

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::prim::*;

/// Serialize an error into an HTTP response
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        use crate::prim::Error::*;

        let response = match self {
            IO(_) => (StatusCode::NOT_FOUND, "Not Found"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
        };

        response.into_response()
    }
}
