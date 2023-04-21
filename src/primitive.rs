//! Primitives and essential dependencies

use std::{
    fmt::{Display, Formatter},
    time::SystemTime,
};

// Reexport or redefine types.

/// UTC DateTime
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime(chrono::DateTime<chrono::Utc>);

impl DateTime {
    /// Get the UTC now
    pub fn now() -> Self {
        Self(chrono::Utc::now())
    }

    /// Return an RFC3339 string (including Z time zone)
    pub fn rfc3339z(&self) -> String {
        self.0.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// Return an RFC2822 string
    pub fn rfc2822(&self) -> String {
        self.0.to_rfc2822()
    }

    /// Parse one from an RFC2822 string
    pub fn from_rfc2822(s: &str) -> Option<Self> {
        chrono::DateTime::parse_from_rfc2822(s)
            .ok()
            .map(|d| Self(d.with_timezone(&chrono::Utc)))
    }

    /// Parse one from a [`SystemTime`] (returned on file statistics calls)
    pub fn from_systemtime(t: SystemTime) -> Option<Self> {
        // Enable chrono::Utc.timestamp_opt.
        // This function converts a pair of seconds and nanoseconds from the
        // SystemTime type to a DateTime type.
        use chrono::TimeZone;

        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| {
                chrono::Utc.timestamp_opt(d.as_secs() as i64, d.subsec_nanos())
            })
            .map(|t| t.single().unwrap())
            .map(Self)
    }
}

/// Anyhow error
pub use anyhow;

/// Tracing
pub use tracing;

/// Unified error type.
///
/// Any time you use [`anyhow::Error`], you can use this type instead.
/// It will do an implicit conversion to [`anyhow::Error`] for you
/// whenever you use the `?` operator.
///
/// The wrapped errors inside are strictly internal. The client will
/// not see any messages or details.
#[derive(Debug, thiserror::Error)]
pub enum UnifiedError {
    /// 500 Internal Server Error
    InternalServerError(
        #[from]
        #[source]
        anyhow::Error,
    ),
    /// 404 Not Found
    NotFound(#[source] anyhow::Error),
}

impl Display for UnifiedError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            UnifiedError::NotFound(e) => write!(f, "Not found: {}", e),
            UnifiedError::InternalServerError(e) => {
                write!(f, "Internal server error: {}", e)
            }
        }
    }
}

impl axum::response::IntoResponse for UnifiedError {
    /// Allow to be rendered as an Axum Response using hard-coded &'static str JSON strings.
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;

        match self {
            UnifiedError::NotFound(_) => {
                (StatusCode::NOT_FOUND, r#"{"status":404}"#).into_response()
            }
            UnifiedError::InternalServerError(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, r#"{"status":500}"#)
                    .into_response()
            }
        }
    }
}

// Allow free conversion of an [`std::io::Error`] into a [`UnifiedError`]
impl From<std::io::Error> for UnifiedError {
    fn from(e: std::io::Error) -> Self {
        UnifiedError::NotFound(anyhow::anyhow!(e))
    }
}

/// Unified Result. You can return this type directly in an
/// Axum endpoint handler. It will return valid JSON responses
/// when there is an error with the correct status code.
pub type Result<T> = std::result::Result<T, UnifiedError>;
