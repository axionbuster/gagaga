//! Define domain-specific types and processes
use std::{
    fmt::Display,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Context;
use chrono::TimeZone;
use rand::seq::SliceRandom;
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
            "Path {path:?} is not a subpath of the working \
directory {workdir:?}"
        )));
    }

    Ok(ResolvedPath(path))
}

/// Produce a JSON response from the directory listing that can be
/// consumed by the frontend.
pub async fn dirlistjson<const N: usize>(
    path: &ResolvedPath,
    parent_path: &ResolvedPath,
) -> Result<String> {
    const API_VERSION: &str = "011";
    let mut n_entries = 0;
    let mut readdir = tokio::fs::read_dir(path.as_ref()).await?;
    let mut rootobject = serde_json::Map::new();

    // Add the API version
    rootobject.insert("version".to_string(), API_VERSION.into());

    // Files and directories are collected separately, then shuffled.
    let mut files = Vec::new();
    let mut directories = Vec::new();

    // Traverse the directory entries
    while n_entries < N {
        n_entries += 1;
        let entry = readdir.next_entry().await;
        if entry.is_err() {
            continue;
        }
        let entry = entry.unwrap();
        // Check for end of iteration
        if entry.is_none() {
            break;
        }
        let entry = entry.unwrap();
        // Retrieve the full path
        let path = entry.path();
        // Get the file metadata
        let metadata = tokio::fs::metadata(&path).await;
        if metadata.is_err() {
            continue;
        }
        let metadata = metadata.unwrap();
        // Make sure either a link, file or a directory
        if !metadata.file_type().is_symlink()
            && !metadata.file_type().is_file()
            && !metadata.file_type().is_dir()
        {
            continue;
        }
        // Last modified
        let last_modified = metadata
            .modified()
            .ok()
            .and_then(systime2datetime)
            .map(|dt| {
                dt.to_rfc3339_opts(
                    chrono::SecondsFormat::Secs,
                    // Use Z+.
                    true,
                )
            });
        // File name
        let name = entry.file_name().to_string_lossy().to_string();
        // Path relative to the working directory
        let relpath = path.strip_prefix(parent_path.as_ref());
        if relpath.is_err() {
            continue;
        }
        let relpath = relpath.unwrap();
        // User URL. If dir, /user/{}. If file, /root/{}.
        let url = if metadata.is_dir() {
            Path::new("/user").join(relpath)
        } else {
            Path::new("/root").join(relpath)
        };
        // Decide custom thumbnail by file extension
        let custom_thumb = path
            .extension()
            .map(|s| {
                s.to_str()
                    .map(|s| matches!(s, "png" | "jpg" | "jpeg" | "gif"))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        // Thumb URL. If no custom thumbnail and if dir, /thumbdir, if file, /thumb.
        // If custom thumbnail, /thumb/{}.
        let thumb_url = if custom_thumb {
            Path::new("/thumb").join(relpath)
        } else if metadata.is_dir() {
            Path::new("/thumbdir").to_path_buf()
        } else {
            Path::new("/thumb").to_path_buf()
        };
        // Serialize
        let entryobject = serde_json::json!({
            "name": name,
            "url": url.to_string_lossy(),
            "thumb_url": thumb_url.to_string_lossy(),
            "last_modified": last_modified,
        });
        // Add to the appropriate list
        if metadata.is_dir() {
            directories.push(entryobject);
        } else {
            files.push(entryobject);
        }
    }

    // Shuffle the arrays
    let mut rng = rand::thread_rng();
    files.shuffle(&mut rng);
    directories.shuffle(&mut rng);

    // Insert
    rootobject.insert("files".to_string(), files.into());
    rootobject.insert("directories".to_string(), directories.into());

    // Check for truncation
    rootobject.insert("truncated".to_string(), (n_entries >= N).into());

    // Serialize
    let json = serde_json::to_string(&rootobject)
        .context("dirlistjson: can't serialize")?;

    Ok(json)
}
