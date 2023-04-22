//! All the primitives, gather here.
//!
//! - Error and Result
//! - Time handling
use std::time::SystemTime;

pub use anyhow::Context;
pub use tracing::instrument;

use thiserror::Error;
use time::OffsetDateTime;

/// Unified Error type
#[non_exhaustive]
#[derive(Error, Debug)]
pub enum Error {
    /// Any error that is not covered by the other variants
    #[error("General Error: {0}")]
    General(
        #[source]
        #[from]
        anyhow::Error,
    ),

    /// Any error caused by I/O
    #[error("I/O Error: {0}")]
    IO(#[source] anyhow::Error),

    /// Any error caused while parsing times and dates
    #[error("Time Error: {0}")]
    TimeError(#[source] anyhow::Error),
}

/// Allow smooth conversion from an [`std::io::Error`] to
/// [`Error::IO`]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::IO(e.into())
    }
}

/// General result type
pub type Result<T> = std::result::Result<T, Error>;

/// UTC Time and Date
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime(OffsetDateTime);

impl DateTime {
    /// Now
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// RFC3339 with Zulu
    pub fn rfc3339z(&self) -> String {
        use time::format_description::well_known::Rfc3339;

        self.0
            .format(&Rfc3339)
            .context("formatting date to RFC3339")
            .map_err(Error::TimeError)
            .unwrap()
    }

    /// RFC2822 (used in HTTP)
    pub fn rfc2822(&self) -> String {
        use time::format_description::well_known::Rfc2822;

        self.0
            .format(&Rfc2822)
            .context("formatting date to RFC2822")
            .map_err(Error::TimeError)
            .unwrap()
    }

    /// From RFC2822 (used in HTTP)
    pub fn from_rfc2822(s: impl AsRef<str>) -> Result<Self> {
        use time::format_description::well_known::Rfc2822;

        let time = OffsetDateTime::parse(s.as_ref(), &Rfc2822)
            .context("parsing RFC2822")
            .map_err(Error::TimeError)?;

        Ok(Self(time))
    }

    /// From [`SystemTime`] (used by Rust)
    pub fn from_system_time(st: &SystemTime) -> Self {
        Self(OffsetDateTime::from(*st))
    }
}

impl From<SystemTime> for DateTime {
    fn from(st: SystemTime) -> Self {
        Self::from_system_time(&st)
    }
}
