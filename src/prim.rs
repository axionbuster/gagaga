//! All the primitives, gather here.
//!
//! - Error and Result
//! - Time handling
//! - Logging and [`macro@instrument`] macro

pub use anyhow::{anyhow, Context};
pub use tracing::instrument;

use std::{cmp::Ordering, fmt::Debug, time::SystemTime};

use httpdate::{fmt_http_date, parse_http_date};
use time::OffsetDateTime;

/// Unified Error Type
pub type Error = anyhow::Error;

/// General result type
pub type Result<T> = std::result::Result<T, Error>;

/// Convenient ways to turn an [`Option`] into a [`Result`]
pub trait OptionExt<T>: Sized {
    /// If the option is [`None`], return an error with a static message
    fn ok_or_err(self, msg: &'static str) -> Result<T>;

    /// If the option is [`None`], return an error created by a
    /// closure
    fn ok_or_err_with<F: FnOnce() -> Error>(self, f: F) -> Result<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn ok_or_err(self, msg: &'static str) -> Result<T> {
        self.ok_or_else(|| anyhow!(msg))
    }

    fn ok_or_err_with<F: FnOnce() -> Error>(self, f: F) -> Result<T> {
        self.ok_or_else(f)
    }
}

/// UTC Time and Date
///
/// # Comparing
///
/// You can use the built-in [`Ord`] and [`PartialOrd`] implementations
/// to compare two `DateTime`s, but they compare every nanosecond
/// if they can.
///
/// If you want to compare down to the second resolution, use
/// [`DateTime::seccmp`], like so:
///
/// ```rust
/// use std::cmp::Ordering;
///
/// use crate::prim::DateTime;
///
/// let a = DateTime::now();
/// let b = DateTime::now();
///
/// // If they execute in the same second, they will be equal.
/// assert_eq!(a.seccmp(&b), Ordering::Equal);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime(OffsetDateTime);

impl DateTime {
    /// The time as of now
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// RFC3339 with Zulu
    #[instrument]
    pub fn rfc3339z(&self) -> String {
        use time::format_description::well_known::Rfc3339;

        self.0
            .format(&Rfc3339)
            .context("formatting date to RFC3339")
            .unwrap()
    }

    /// As used in Last-Modified
    pub fn http(&self) -> String {
        fmt_http_date(self.0.into())
    }

    /// As used in Last-Modified
    #[instrument(err)]
    pub fn from_http(s: impl AsRef<str> + Debug) -> Result<Self> {
        parse_http_date(s.as_ref())
            .map(|time| Self(time.into()))
            .context("parsing HTTP datetime")
    }

    /// From [`SystemTime`] (used by Rust)
    pub fn from_system_time(st: &SystemTime) -> Self {
        Self(OffsetDateTime::from(*st))
    }

    /// Compare down to the second resolution (useful in HTTP)
    pub fn seccmp(&self, other: &Self) -> Ordering {
        self.0.unix_timestamp().cmp(&other.0.unix_timestamp())
    }

    /// Get the signed Unix timestamp (seconds)
    pub fn sgnunixsec(&self) -> i64 {
        self.0.unix_timestamp()
    }
}

impl From<SystemTime> for DateTime {
    fn from(st: SystemTime) -> Self {
        Self::from_system_time(&st)
    }
}
