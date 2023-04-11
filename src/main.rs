use std::path::PathBuf;

use anyhow::Context;
use axum::{response::IntoResponse, routing::get, Router};
use tokio::io::AsyncReadExt;

mod domainprim {
    //! Define domain-specific types and processes
    use std::{
        fmt::Display,
        os::unix::prelude::OsStrExt,
        path::{Path, PathBuf},
    };

    use anyhow::Context;
    use chrono::TimeZone;
    use serde::{ser::SerializeMap, Serialize};

    /// UTC DateTime
    type DateTime = chrono::DateTime<chrono::Utc>;

    /// Attempt to convert a SystemTime (returned on file statistics calls)
    /// to the DateTime type. How inconvenient is this?
    fn systime2datetime(t: std::time::SystemTime) -> Option<DateTime> {
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| chrono::Utc.timestamp_opt(d.as_secs() as i64, d.subsec_nanos()))
            .map(|t| t.single().unwrap())
    }

    /// Unified error type
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
                UnifiedError::InternalServerError(e) => write!(f, "Internal server error: {}", e),
            }
        }
    }

    // Allow to be rendered as an Axum Response using hard-coded &'static str JSON strings.
    impl axum::response::IntoResponse for UnifiedError {
        fn into_response(self) -> axum::response::Response {
            use http::StatusCode;

            match self {
                UnifiedError::NotFound(_) => {
                    (StatusCode::NOT_FOUND, r#"{"status":404}"#).into_response()
                }
                UnifiedError::InternalServerError(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, r#"{"status":500}"#).into_response()
                }
            }
        }
    }

    // Allow free conversion of an std::io::Error into a UnifiedError
    impl From<std::io::Error> for UnifiedError {
        fn from(e: std::io::Error) -> Self {
            UnifiedError::NotFound(anyhow::anyhow!(e))
        }
    }

    /// Unified Result
    pub type Result<T> = std::result::Result<T, UnifiedError>;

    /// An absolute, resolved file path that was trusted when it was created.
    /// This is relative to the server's computer.
    #[derive(Debug, Clone)]
    pub struct ResolvedPath(PathBuf);

    impl ResolvedPath {
        /// Trust a PathBuf with no validation whatsoever
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

    /// Attempt to resolve a path asynchronously and admit it if
    /// it is a subpath of the right path, an absolute, similarly
    /// resolved path.
    pub async fn pathresolve(path: &Path, parent: &ResolvedPath) -> Result<ResolvedPath> {
        // Ask Tokio to resolve the path asynchronously
        let path = tokio::fs::canonicalize(path).await;
        let path: PathBuf = path?;

        // Decide whether the resolved path is a subpath of the parent
        if !path.starts_with(parent.as_ref()) {
            return Err(UnifiedError::NotFound(anyhow::anyhow!(
                "Path is not a subpath of the parent"
            )));
        }

        Ok(ResolvedPath(path))
    }

    /// A file, directory, or similar objects of interest
    pub struct DomainFile {
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

        /// Decide if this is a file
        // pub fn is_file(&self) -> bool {
        //     self.flags & 0b11 == 1
        // }

        /// Decide if this is a directory
        pub fn is_directory(&self) -> bool {
            self.flags & 0b11 == 2
        }

        /// Decide if this has a custom thumbnail
        pub fn has_custom_thumbnail(&self) -> bool {
            self.flags & 0b100 == 4
        }
    }

    // Seralize DomainFile into JSON
    impl Serialize for DomainFile {
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
            let url = format!("/root/{}", rootrelpath);

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
        pub truncated: bool,
        pub files: Vec<DomainFile>,
        pub directories: Vec<DomainFile>,
    }

    // Return an Axum response using the serialized JSON
    impl axum::response::IntoResponse for DomainDirListing {
        fn into_response(self) -> axum::response::Response {
            use axum::body::Body;
            use axum::http::{header, HeaderValue, StatusCode};

            let json = serde_json::to_string(&self);
            if json.is_err() {
                let mut response =
                    http::Response::new(Body::from(r#"{"error":"Internal server error"}"#));
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
            let entry: Option<tokio::fs::DirEntry> = entry?;
            // We are done if the entry is None
            if entry.is_none() {
                break;
            }
            // Call fstat or equivalent and gather metadata.
            let entry = entry.unwrap();
            // Resolve the path. This will also make sure that
            // the true path is a subpath of the parent path.
            let path = entry.path();
            let path = pathresolve(&path, parent_path).await;
            if path.is_err() {
                continue;
            }
            let path: ResolvedPath = path.unwrap();
            // Strip prefix in path
            let server_path = path
                .as_ref()
                .strip_prefix(parent_path)
                .context("strip")
                .map_err(|_e| anyhow::anyhow!("strip prefix path dirlist"))?;
            let server_path = server_path.to_path_buf();

            let metadata = entry.metadata().await?;
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
}

/// Serve a file or directory, downloading if a regular file,
/// or listing if a directory.
async fn serve_user_path(
    userpath: Option<axum::extract::Path<String>>,
) -> domainprim::Result<axum::response::Response> {
    // Domain-specific primitives
    use crate::domainprim::{dirlist, pathresolve, ResolvedPath, UnifiedError::*};

    // Executable's directory. Will refactor to consider other places
    // than just the place where the executable is.
    // FIXME: refactor.
    let rootdir: PathBuf = std::env::current_dir()?;
    let rootdir = ResolvedPath::from_trusted_pathbuf(rootdir);

    // If the user didn't provide a path, then serve the root directory.
    let user: PathBuf = if userpath.is_none() {
        // FIXME: Use a given root path instead of the executable's directory
        // ("./").
        PathBuf::from("./")
    } else {
        PathBuf::from(userpath.unwrap().as_str())
    };

    // Resolve the path (convert user's path to server's absolute path, as well as
    // following symlinks and all that). Note: according to the contract of
    // ResolvedPath, it's guaranteed to be absolute and within the root directory.
    let userpathreal = pathresolve(&user, &rootdir).await?;

    // Check if the path points to a directory or a file.
    let filemetadata = userpathreal.as_ref().metadata()?;

    // If it's a regular file, then download it.
    if filemetadata.is_file() {
        // download the file

        // First, let Tokio read it asynchronously.
        let mut file = tokio::fs::File::open(userpathreal.as_ref()).await?;
        // Read everything into a Vec<u8>.
        let mut buf = vec![];
        file.read_to_end(&mut buf).await?;
        // Axum: make a response.
        let response = axum::response::Response::builder()
            .header("Content-Type", "application/octet-stream")
            .body(axum::body::Body::from(buf))
            .context("file send make response")?;
        let response = response.into_response();

        return Ok(response);
    }

    // If it's not a directory, then say, not found.
    if !filemetadata.is_dir() {
        return Err(NotFound(anyhow::anyhow!(
            "serve_user_path: neither a file nor a directory"
        )));
    }

    // A directory. List it with a limit of 3000 files.
    let list = dirlist::<3000>(
        // path (user's control)
        &userpathreal,
        // don't go outside of the root directory (server's control)
        &rootdir,
    )
    .await?;

    let response = list.into_response();

    Ok(response)
}

/// SVG Icon for folder, Font Awesome.
const SVG_FOLDER: &str = include_str!("folder-solid.svg");

/// SVG Icon for file, Font Awesome.
const SVG_FILE: &str = include_str!("file-solid.svg");

/// Serve a static SVG file
async fn serve_svg(svg: &'static str) -> axum::response::Response {
    let response = axum::response::Response::builder()
        .header("Content-Type", "image/svg+xml")
        .body(axum::body::Body::from(svg))
        .context("svg send make response")
        .unwrap();
    response.into_response()
}

/// Serve a specific thumbnail in JPEG format where possible.
/// If the thumbnail is not available, then serve a default thumbnail.
///
/// Preserve aspect ratio while fitting in TWxTH.
async fn serve_thumb<const TW: u32, const TH: u32>(
    userpath: axum::extract::Path<String>,
) -> domainprim::Result<axum::response::Response> {
    // Domain-specific primitives
    use crate::domainprim::{pathresolve, ResolvedPath};

    // Executable's directory. Will refactor to consider other places
    // than just the place where the executable is.
    // FIXME: refactor.
    let rootdir: PathBuf = std::env::current_dir()?;
    let rootdir = ResolvedPath::from_trusted_pathbuf(rootdir);

    let user = PathBuf::from(userpath.as_str());

    // Resolve the path (convert user's path to server's absolute path, as well as
    // following symlinks and all that). Note: according to the contract of
    // ResolvedPath, it's guaranteed to be absolute and within the root directory.
    let userpathreal = pathresolve(&user, &rootdir).await?;

    // Check if the path points to a directory or a file.
    let filemetadata = userpathreal.as_ref().metadata()?;

    // If directory, then serve the folder icon.
    if filemetadata.is_dir() {
        return Ok(serve_svg(SVG_FOLDER).await);
    }

    // If not file, reject. Serve the file icon.
    if !filemetadata.is_file() {
        return Ok(serve_svg(SVG_FILE).await);
    }

    // A file. Generate a thumbnail. If successful, serve it.
    // Otherwise, serve the file icon.
    let thumb: Vec<u8> = match gen_thumb::<TW, TH>(userpathreal).await {
        Ok(value) => value,
        Err(_value) => return Ok(serve_svg(SVG_FILE).await),
    };
    let response = axum::response::Response::builder()
        .header("Content-Type", "image/jpeg")
        .body(axum::body::Body::from(thumb))
        .context("thumb jpeg send make response")?
        .into_response();
    Ok(response)
}

/// Attempt to generate a thumbnail in JPEG format with hard-coded quality.
///
/// Because the generation can take a long time, it is delegated in a blocking
/// thread using Tokio.
async fn gen_thumb<const TW: u32, const TH: u32>(
    userpathreal: domainprim::ResolvedPath,
) -> domainprim::Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(userpathreal.as_ref()).await?;
    let mut buf = vec![];
    file.read_to_end(&mut buf).await?;
    let cursor = std::io::Cursor::new(buf);
    // Sync block
    let join = tokio::task::spawn_blocking(move || {
        let img = image::io::Reader::new(cursor);
        let img = img.with_guessed_format()?;
        let img = img.decode().context("gen_thumb: cannot decode image")?;
        let img = img.thumbnail(TW, TH);
        let format = image::ImageOutputFormat::Jpeg(50);
        let mut cursor = std::io::Cursor::new(vec![]);
        img.write_to(&mut cursor, format)
            .context("gen_thumb: cannot write image")?;
        Ok(cursor.into_inner())
    });
    join.await.context("gen_thumb: thread join fail")?
}

#[tokio::main]
async fn main() {
    // Set up logging
    tracing_subscriber::fmt::init();

    // Build app
    let app = Router::new()
        .route("/root", get(|| async { serve_user_path(None).await }))
        .route("/root/*userpath", get(serve_user_path))
        .route("/thumb", get(|| async { serve_svg(SVG_FILE).await }))
        .route("/thumbdir", get(|| async { serve_svg(SVG_FOLDER).await }))
        .route("/thumb/*userpath", get(serve_thumb::<200, 200>));

    // Start server, listening on port 3000
    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
