//! Define domain-specific types and processes
use std::{
    fmt::Display,
    os::unix::prelude::OsStrExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::TimeZone;
use serde::{ser::SerializeMap, Serialize};
use tracing::instrument;

/// UTC DateTime
pub type DateTime = chrono::DateTime<chrono::Utc>;

/// Attempt to convert a [`SystemTime`] (returned on file statistics calls)
/// to the DateTime type. How inconvenient is this?
pub fn systime2datetime(t: SystemTime) -> Option<DateTime> {
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| {
            chrono::Utc.timestamp_opt(d.as_secs() as i64, d.subsec_nanos())
        })
        .map(|t| t.single().unwrap())
}

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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        use http::StatusCode;

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

/// An absolute, resolved file path that was trusted when it was created.
/// This is relative to the server's computer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedPath(PathBuf);

impl ResolvedPath {
    /// Trust a PathBuf with no validation whatsoever.
    ///
    /// This should be used only when the path is known to be
    /// absolute and resolved, and is not user input.
    ///
    /// Hence, no [`From`] implementation is provided.
    pub fn from_trusted_pathbuf(path: PathBuf) -> ResolvedPath {
        ResolvedPath(path)
    }
}

/// Expose the &Path reference
impl AsRef<Path> for ResolvedPath {
    fn as_ref(&self) -> &Path {
        self.0.as_ref()
    }
}

/// Given a normal, relative path (to the given working directory),
/// attempt to canonicalize it by following symlinks and resolving
/// paths, producing an absolute path.
#[instrument(err, level = "debug")]
pub async fn pathresolve(
    pathuser: &Path,
    workdir: &ResolvedPath,
) -> Result<ResolvedPath> {
    // Do a quick check to see if the path is "normal"
    if !pathuser.components().all(|c| {
        matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::RootDir
        )
    }) {
        return Err(UnifiedError::NotFound(anyhow::anyhow!(
            "Path {pathuser:?} is not a normal path"
        )));
    }

    // Ask Tokio to resolve the path asynchronously
    let meantpath = workdir.as_ref().join(pathuser);
    let path = tokio::fs::canonicalize(meantpath).await?;

    // Decide whether the resolved path is a subpath of the parent
    if !path.starts_with(workdir.as_ref()) {
        return Err(UnifiedError::NotFound(anyhow::anyhow!(
            "Path {path:?} is not a subpath of the working directory {workdir:?}"
        )));
    }

    Ok(ResolvedPath(path))
}

/// A file, directory, or similar objects of interest as understood
/// by the server.
struct DomainFile {
    /// The file as found on the server, relative to the servicing directory
    pub server_path: PathBuf,
    /// Metadata: last modified time, if available
    pub last_modified: Option<DateTime>,
    /// Metadata: type of file:
    /// bit 0-1: 0 = unknown, 1 = file, 2 = directory
    /// bit 2: 0 = default thumbnail, 1 = custom thumbnail
    flags: u8,
}

impl DomainFile {
    /// Create a new DomainFile
    pub fn new(
        server_path: PathBuf,
        last_modified: Option<DateTime>,
        is_file: Option<bool>,
        custom_thumbnail: bool,
    ) -> Self {
        // Currently, the file type must be known to be one of
        // file (Some(true)) or directory (Some(false)).
        // If I become unaware and introduce a new feature without
        // updating this, I will get a crash.
        if is_file.is_none() {
            todo!(
                "In DomainFile::new, is_file is None (unknown type file).
Currently, only file (Some(true)) or directory (Some(false)) are supported."
            );
        }

        DomainFile {
            server_path,
            last_modified,
            flags: match is_file {
                Some(true) => 1,
                Some(false) => 2,
                None => 0,
            } | if custom_thumbnail { 4 } else { 0 },
        }
    }

    /// Decide if this is a directory
    pub fn is_directory(&self) -> bool {
        self.flags & 0b11 == 2
    }

    /// Decide if this has a custom thumbnail
    pub fn has_custom_thumbnail(&self) -> bool {
        self.flags & 0b100 == 4
    }
}

impl Serialize for DomainFile {
    /// Note: For certain malformed paths, this may return an empty JSON object.
    /// See the code for details.
    ///
    /// I also don't know if this is the best way to serialize this.
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_map(None)?;
        // Display name
        let name: &Path = self.server_path.as_ref();
        let name: Option<&std::ffi::OsStr> = name.file_name();

        if name.is_none() {
            // "", "..", and "/" will have no name.
            // Among these, only "/" should be possible to encounter.
            // In any case, we don't want to serialize them.
            // Leave empty.
            return state.end();
        }

        let name = name.unwrap().to_string_lossy();

        // Client's URL to view or download
        let fullpath = self.server_path.to_str();
        if fullpath.is_none() {
            // This should not happen most of the time.
            return state.end();
        }
        let rootrelpath = fullpath.unwrap();
        // If directory, go to "/user". This allows browsing.
        // If file, go to "/root". This allows downloading.
        let url = if self.is_directory() {
            format!("/user/{}", rootrelpath)
        } else {
            format!("/root/{}", rootrelpath)
        };

        // Client's thumbnail URL to view or download
        // Synopsis: if no custom thumbnail, then use "/thumbdir" for directories
        // and "/thumb" for any other. If custom thumbnail, then use
        // "/thumb/{}" (with the path).
        let thumb_url = if self.has_custom_thumbnail() {
            format!("/thumb/{}", rootrelpath)
        } else if self.is_directory() {
            "/thumbdir".to_string()
        } else {
            "/thumb".to_string()
        };

        // Last Modified in RFC3339 / ISO8601 format
        let last_modified = self.last_modified.map(|t| {
            t.to_rfc3339_opts(
                chrono::SecondsFormat::Secs,
                // Use "Z+" in the timezone
                true,
            )
        });

        // Serialize
        state.serialize_entry("name", &name)?;
        state.serialize_entry("url", &url)?;
        state.serialize_entry("thumb_url", &thumb_url)?;
        if let Some(last_modified) = last_modified {
            state.serialize_entry("last_modified", &last_modified)?;
        }
        // Note: we don't serialize the "flags" field
        state.end()
    }
}

/// A directory listing
#[derive(Serialize)]
pub struct DomainDirListing {
    /// Did this listing get truncated because more than a certain
    /// number of files were found?
    truncated: bool,
    /// The files in this directory, once resolved.
    files: Vec<DomainFile>,
    /// The directories in this directory, once resolved.
    directories: Vec<DomainFile>,
}

// Return an Axum response using the serialized JSON
impl axum::response::IntoResponse for DomainDirListing {
    /// We specifically create a JSON response.
    fn into_response(self) -> axum::response::Response {
        use axum::body::Body;
        use axum::http::{header, HeaderValue, StatusCode};

        let json = serde_json::to_string(&self);
        if json.is_err() {
            let mut response = http::Response::new(Body::from(
                r#"{"error":"Internal server error"}"#,
            ));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            return response.into_response();
        }
        let json = json.unwrap();
        let body = Body::from(json);
        let mut response = http::Response::new(body);
        *response.status_mut() = StatusCode::OK;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        response.into_response()
    }
}

/// Walk a directory and collect into two vectors:
/// - directories
/// - files
///
/// A hard-coded limit of N entries apply. If the limit is reached,
/// then the limit_reached flag is set to true.
#[instrument(err)]
pub async fn dirlist<const N: usize>(
    path: &ResolvedPath,
    parent_path: &ResolvedPath,
) -> Result<DomainDirListing> {
    let mut domaindir = DomainDirListing {
        files: Vec::new(),
        directories: Vec::new(),
        truncated: false,
    };

    // Walk the directory, while keeping track of the number of
    // entries we have seen.
    let mut n = 0;
    let mut readdir = tokio::fs::read_dir(path.as_ref()).await?;
    while n < N {
        n += 1;
        let entry = readdir.next_entry().await;

        // If the entry listing fails for this item, then
        // just skip it.
        if entry.is_err() {
            continue;
        }
        let entry: Option<tokio::fs::DirEntry> = entry.unwrap();
        // We are done if the entry is None
        if entry.is_none() {
            break;
        }
        let entry = entry.unwrap();
        // Resolve the path. This will also make sure that
        // the true path is a subpath of the parent path.
        let path = entry.path();
        let path = pathresolve(&path, parent_path).await;
        if path.is_err() {
            continue;
        }
        let path: ResolvedPath = path.unwrap();

        // Strip prefix in path to get the server path.
        // The server path is the path relative to the parent path.
        // Eventually, it gets converted to URLs and display names
        // downstream.
        //
        // FIXME: I think it's unnecessary to do this. Since the
        // DirEntry has custom serialization that guards against
        // the full path being shown anyway, maybe we could do it
        // at the time of serialization. There's also a potential
        // that we could avoid extra memory allocation doing it
        // like that. But I'm not sure. It's working for now.
        let server_path = path
            .as_ref()
            .strip_prefix(parent_path)
            .map(|p| p.to_path_buf());
        if server_path.is_err() {
            // If there was an error while stripping the prefix,
            // then just skip this entry.
            // Though, I gotta say, that would be pretty weird.
            // Let's log that.
            tracing::warn!("Could not strip prefix in path: {path:?}; That's weird. Ignoring directory entry.");
            continue;
        }
        let server_path = server_path.unwrap();

        // Call fstat or equivalent and gather metadata.
        // At least we wanna know the last modified time.
        // And if my code changes in the future, maybe more.
        let metadata = entry.metadata().await;
        if metadata.is_err() {
            continue;
        }
        let metadata = metadata.unwrap();
        let last_modified: Option<DateTime> =
            metadata.modified().ok().and_then(systime2datetime);

        // Decide whether the entry is a directory or a file, and then
        // put it in the appropriate vector.
        if metadata.is_dir() {
            domaindir.directories.push(DomainFile::new(
                server_path,
                last_modified,
                // This is a directory.
                Some(false),
                false,
            ));
        } else if metadata.is_file() {
            // Decide whether the file should have a custom thumbnail
            let use_custom_thumbnail = extjpeg(path.as_ref());

            domaindir.files.push(DomainFile::new(
                server_path,
                last_modified,
                // This is a file.
                Some(true),
                use_custom_thumbnail,
            ));
        } else {
            // Do nothing if it's neither a file nor a directory.
            // Stuff like devices.
        }
    }

    // If the limit is reached, then set the flag
    if n == N {
        domaindir.truncated = true;
    }

    // Sort by name, both, from latest to earliest modification times.
    // Try not to expose the underlying filesystem's ordering.
    domaindir
        .directories
        .sort_unstable_by(|a, b| b.last_modified.cmp(&a.last_modified));
    domaindir
        .files
        .sort_unstable_by(|a, b| b.last_modified.cmp(&a.last_modified));

    Ok(domaindir)
}

/// By looking at the extension of a &Path only, heuristically decide whether
/// the file might be one of the thumbnail-supported JPEG file.
/// I will refactor this code to support more than just JPEG. But, for now,
/// I'm going to delegate the flexibility to the human programmer.
fn extjpeg(path: &Path) -> bool {
    let ext = path.extension();
    if ext.is_none() {
        return false;
    }
    let ext = ext.unwrap();
    let extu8 = ext.as_bytes();

    // If the extension is one of the following, then
    // we will use the custom thumbnail.
    // It's really dumb, but it works without allocating.
    matches!(
        extu8,
        b"jpg"
            | b"jpG"
            | b"jPg"
            | b"jPG"
            | b"Jpg"
            | b"JpG"
            | b"JPg"
            | b"JPG"
            | b"jpeg"
            | b"jpeG"
            | b"jpEg"
            | b"jpEG"
            | b"jPeg"
            | b"jPeG"
            | b"jPEg"
            | b"jPEG"
            | b"Jpeg"
            | b"JpeG"
            | b"JpEg"
            | b"JpEG"
            | b"JPeg"
            | b"JPeG"
            | b"JPEg"
            | b"JPEG"
    )
}
