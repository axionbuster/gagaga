use std::path::PathBuf;

use anyhow::Context;
use axum::{
    middleware::{map_request, map_response},
    response::{IntoResponse, Redirect},
    routing::get,
    Router,
};
use tokio::{io::AsyncReadExt, sync::OnceCell};
use tracing::instrument;

mod domainprim {
    //! Define domain-specific types and processes
    use std::{
        fmt::Display,
        os::unix::prelude::OsStrExt,
        path::{Path, PathBuf},
    };

    use chrono::TimeZone;
    use serde::{ser::SerializeMap, Serialize};
    use tracing::instrument;

    /// UTC DateTime
    pub type DateTime = chrono::DateTime<chrono::Utc>;

    /// Attempt to convert a SystemTime (returned on file statistics calls)
    /// to the DateTime type. How inconvenient is this?
    pub fn systime2datetime(t: std::time::SystemTime) -> Option<DateTime> {
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| {
                chrono::Utc.timestamp_opt(d.as_secs() as i64, d.subsec_nanos())
            })
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
                UnifiedError::InternalServerError(e) => {
                    write!(f, "Internal server error: {}", e)
                }
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
                    (StatusCode::INTERNAL_SERVER_ERROR, r#"{"status":500}"#)
                        .into_response()
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
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

    /// Attempt to canonicalize path (`pathuser`) if found.
    /// Once resolved, compare the canonical
    /// path to the parent path. If outside, return an error.
    /// Otherwise, return the absolute, canonicalized path.
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

    /// A file, directory, or similar objects of interest
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
        truncated: bool,
        files: Vec<DomainFile>,
        directories: Vec<DomainFile>,
    }

    // Return an Axum response using the serialized JSON
    impl axum::response::IntoResponse for DomainDirListing {
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
}

mod cachethumb {
    //! Cache some thumbnails!!!
    //!
    //! To issue an instruction to the cache manager, construct
    //! an appropriate type of message, and send it to the
    //! cache manager's channel.
    //!
    //! You are responsible for:
    //! (1) spawning the cache manager process (it's a logical process),
    //! (2) composing messages to the cache manager process.
    //!
    //! The cache manager process will spawn a background task
    //! to inspect the file system to determine freshness and
    //! order all the cache operations.
    //!
    //! Why do this:
    //! - The data structure, HashMap, is single threaded, which
    //! requires the serialization of all instructions.
    //! - I find it simpler than having to deal with concurrency
    //! hazards myself in other places than this.
    //! - It's concurrent anyway so it's hard.
    //!
    //! Example:
    //! - Call spawn_cache_process() to get a channel to the cache manager.
    //! It also spawns it!
    //! - Compose CacheMessage::Insert(...) to insert a new thumbnail.
    //! Send it to the channel returned by the previous step.
    //! - Compose CacheMessage::Get(...) to get a thumbnail. Send it.
    //!
    //! For each one of these cases above, respectively, you can use:
    //! - [`spawn_cache_process`]
    //! - [`ins`]
    //! - [`xget`]

    use tokio::sync::mpsc;
    use tracing::instrument;

    use crate::domainprim::{systime2datetime, DateTime, ResolvedPath};

    use std::collections::HashMap;

    /// Thumbnail with its last modified time.
    ///
    /// Useful for HTTP caching.
    #[derive(Debug)]
    pub struct CacheResponse {
        /// Last-Modified.
        ///
        /// (If caching works, must exist.)
        pub lastmod: DateTime,

        /// Thumbnail, JPEG.
        ///
        /// You can send this directly to the client.
        pub thumbnail: Vec<u8>,
    }

    /// A message to the cache manager "process" (logical).
    #[derive(Debug)]
    enum CacheMessage {
        /// Insert a new thumbnail (Vec<u8>) now.
        Insert(ResolvedPath, Vec<u8>),
        /// Get a thumbnail (Vec<u8>) now only if fresh.
        ///
        /// The manager will inspect the file system asynchronously to
        /// determine freshness if necessary.
        Get(
            ResolvedPath,
            tokio::sync::oneshot::Sender<Option<CacheResponse>>,
        ),
    }

    /// A channel to the cache manager "process" (logical).
    #[derive(Debug)]
    pub struct Mpsc(mpsc::UnboundedSender<CacheMessage>);

    impl Mpsc {
        /// Insert a new thumbnail (Vec<u8>) now.
        pub fn ins(&self, path: &ResolvedPath, data: Vec<u8>) {
            self.0
                .send(CacheMessage::Insert(path.clone(), data))
                .unwrap()
        }

        /// Get a thumbnail (Vec<u8>) now only if fresh.
        ///
        /// "x" stands for "extra" --- that means be careful about the interactions.
        pub async fn get(&self, path: &ResolvedPath) -> Option<CacheResponse> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.0.send(CacheMessage::Get(path.clone(), tx)).unwrap();
            rx.await.unwrap()
        }
    }

    /// A cache manager "process" (logical). It's defined by an implicit
    /// main loop, and it's not a real OS process. But whatever.
    /// For each message, use a one shot channel to communicate.
    #[instrument]
    pub fn spawn_cache_process() -> Mpsc {
        // Define the main loop and spawn it, too.
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            // Global data structure
            let mut cache: HashMap<ResolvedPath, (DateTime, Vec<u8>)> =
                HashMap::new();
            // Event loop
            while let Some(msg) = rx.recv().await {
                match msg {
                    CacheMessage::Insert(path, data) => {
                        tracing::trace!("Got insert");
                        let now = chrono::Utc::now();
                        cache.insert(path, (now, data));
                    }
                    CacheMessage::Get(path, reply_to) => {
                        // Inspect the hashmap and then the filesystem to determine freshness.
                        // If fresh, send the blob (Vec<u8>) back to (reply_to).
                        // Otherwise, send None back to (reply_to).

                        if !cache.contains_key(&path) {
                            tracing::trace!("Get {path:?} was not in cache");
                            reply_to.send(None).unwrap();
                            continue;
                        }

                        // Now, inspect the filesystem.
                        let metadata = tokio::fs::metadata(path.as_ref()).await;
                        // For any I/O errors, just ignore it quietly.
                        if metadata.is_err() {
                            tracing::debug!(
                                "Get {path:?} was 'stale' (I/O error)"
                            );
                            reply_to.send(None).unwrap();
                            continue;
                        }
                        let metadata = metadata.unwrap();
                        let flastmod =
                            metadata.modified().ok().and_then(systime2datetime);
                        // If can't get the modification time, then just ignore it quietly.
                        if flastmod.is_none() {
                            tracing::debug!(
                                "Get {path:?} was 'stale' (no lastmod in fs)"
                            );
                            reply_to.send(None).unwrap();
                            continue;
                        }
                        let flastmod = flastmod.unwrap();

                        // Compare against memory.
                        let (clastmod, data) = cache.get(&path).unwrap();
                        let clastmod = *clastmod;

                        // Decide.
                        if flastmod > clastmod {
                            // Stale
                            tracing::trace!(
                                "Get {path:?} was stale (fs > cache)"
                            );
                            reply_to.send(None).unwrap();
                        } else {
                            // Fresh
                            tracing::trace!("Get {path:?} was fresh");
                            reply_to
                                .send(Some(CacheResponse {
                                    lastmod: clastmod,
                                    thumbnail: data.clone(),
                                }))
                                .unwrap();
                        }

                        continue;
                    }
                }
            }
        });
        Mpsc(tx)
    }
}

/// The root directory of the server.
static ROOT: OnceCell<domainprim::ResolvedPath> = OnceCell::const_new();

/// A middleware to resolve the path.
#[instrument(err, skip(request), fields(path = %userpath.as_ref().map(|x| x.as_str()).unwrap_or("/")))]
async fn resolve_path<B: std::fmt::Debug>(
    userpath: Option<axum::extract::Path<String>>,
    mut request: axum::http::Request<B>,
) -> domainprim::Result<axum::http::Request<B>> {
    // Domain-specific primitives
    use crate::domainprim::{pathresolve, ResolvedPath};

    use std::fs::Metadata;

    let rootdir = ROOT.get().unwrap();

    // If the user didn't provide a path, then serve the root directory.
    let user: PathBuf = if userpath.is_none() {
        PathBuf::from(rootdir.as_ref())
    } else {
        let userpath = userpath.unwrap();
        let userpath = userpath.as_str();
        if userpath == "/" {
            PathBuf::from(rootdir.as_ref())
        } else {
            PathBuf::from(userpath)
        }
    };
    tracing::trace!("Calc path: {user:?}");

    // Resolve the path (convert to absolute, normalize, etc.)
    let resolved: ResolvedPath = pathresolve(&user, rootdir).await?;
    tracing::trace!("Resolved path: {:?}", resolved);
    request.extensions_mut().insert(resolved.clone());

    // Find the metadata, too, by running fstat (or equivalent).
    let metadata: Option<Metadata> = tokio::fs::metadata(resolved).await.ok();
    tracing::trace!("Metadata: {:?}", metadata);
    request.extensions_mut().insert(metadata);

    Ok(request)
}

/// Serve a file or directory, downloading if a regular file,
/// or listing if a directory.
#[instrument(err, skip(request))]
async fn serve_root<B: std::fmt::Debug>(
    request: axum::http::Request<B>,
) -> domainprim::Result<axum::response::Response> {
    // Domain-specific primitives
    use crate::domainprim::{dirlist, UnifiedError::*};

    // Get the resolved path from the request.
    let userpathreal = request
        .extensions()
        .get::<domainprim::ResolvedPath>()
        .unwrap()
        .clone();
    let filemetadata = request
        .extensions()
        .get::<Option<std::fs::Metadata>>()
        .unwrap()
        .clone();

    // If the metadata could not be fetched, then, say, not found.
    if filemetadata.is_none() {
        return Err(NotFound(anyhow::anyhow!(
            "metadata for {:?} could not be fetched",
            userpathreal
        )));
    }
    let filemetadata = filemetadata.unwrap();

    // If it's a regular file, then download it.
    if filemetadata.is_file() {
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
        ROOT.get().unwrap(),
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
#[instrument]
async fn serve_svg(svg: &'static str) -> axum::response::Response {
    let response = axum::response::Response::builder()
        .header("Content-Type", "image/svg+xml")
        .body(axum::body::Body::from(svg))
        .context("svg send make response")
        .unwrap();
    response.into_response()
}

/// Serve image loading placeholder PNG file
#[instrument]
async fn serve_loading_png() -> &'static [u8] {
    // Font Awesome: image
    include_bytes!("image-solid.png")
}

/// Add static assets caching with public, max-age=(1 hour).
#[instrument]
async fn add_static_cache_control(
    mut response: axum::response::Response,
) -> axum::response::Response {
    response.headers_mut().insert(
        "Cache-Control",
        axum::http::HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

/// A thumbnail cache, shared between all threads, a channel.
static CACHEMPSC: OnceCell<cachethumb::Mpsc> = OnceCell::const_new();

/// Serve a specific thumbnail in JPEG format where possible.
/// If the thumbnail is not available, then serve a default thumbnail.
///
/// Preserve aspect ratio while fitting in TWxTH.
#[instrument(err)]
async fn serve_thumb<B: std::fmt::Debug, const TW: u32, const TH: u32>(
    headers: axum::http::HeaderMap,
    request: axum::http::Request<B>,
) -> domainprim::Result<axum::response::Response> {
    // Domain-specific primitives
    use crate::domainprim::UnifiedError::*;

    // Get the resolved path & metadata from the request.
    let userpathreal = request
        .extensions()
        .get::<domainprim::ResolvedPath>()
        .unwrap()
        .clone();
    let filemetadata = request
        .extensions()
        .get::<Option<std::fs::Metadata>>()
        .unwrap()
        .clone();

    // If the metadata could not be fetched, then serve the file icon.
    if filemetadata.is_none() {
        return Ok(serve_svg(SVG_FILE).await);
    }
    let filemetadata = filemetadata.unwrap();

    // If directory, then serve the folder icon.
    if filemetadata.is_dir() {
        return Ok(serve_svg(SVG_FOLDER).await);
    }

    // If not file, reject.
    if !filemetadata.is_file() {
        return Err(NotFound(anyhow::anyhow!(
            "serve_thumb: neither a file nor a directory"
        )));
    }

    // A file. Generate a thumbnail. If successful, serve it.
    // Otherwise, serve the file icon.

    // See if we got a fresh one.
    let cache = CACHEMPSC.get().unwrap();
    let thumb_recall = cache.get(&userpathreal).await;

    // TODO: Refactor. What is this?
    if headers.contains_key(axum::http::header::IF_MODIFIED_SINCE)
        && thumb_recall.is_some()
    {
        // Decide if we should send a 304 Not Modified.
        let clastmod = &thumb_recall.as_ref().unwrap().lastmod;
        let ulastmod = headers.get(axum::http::header::IF_MODIFIED_SINCE);
        if ulastmod.is_some() {
            // Still deciding...
            let ulastmod = ulastmod.unwrap().to_str();
            if ulastmod.is_ok() {
                // Still deciding...
                let ulastmod = ulastmod.unwrap();
                let ulastmod =
                    chrono::DateTime::parse_from_rfc2822(ulastmod).unwrap();
                let send = clastmod > &ulastmod;

                if send {
                    let response = axum::response::Response::builder()
                        .status(304)
                        .body(axum::body::Body::empty())
                        .context("thumb send 303 make response")
                        .unwrap();
                    return Ok(response.into_response());
                }
            }
        }
    }

    let thumb = match thumb_recall {
        // Fresh
        Some(thumb) => thumb,
        // Stale or nonexistent
        None => {
            // Make a thumbnail.
            let thumb = match gen_thumb::<TW, TH>(&userpathreal).await {
                Ok(value) => value,
                // Quietly ignore errors in this step.
                Err(e) => {
                    tracing::warn!(
                        "thumb gen error (ignored, sent generic): {e}"
                    );
                    return Ok(serve_svg(SVG_FILE).await);
                }
            };
            // Send the thumbnail to the cache.
            cache.ins(&userpathreal, thumb.clone());
            cachethumb::CacheResponse {
                lastmod: chrono::Utc::now(),
                thumbnail: thumb,
            }
        }
    };

    let response = axum::response::Response::builder()
        .header("Content-Type", "image/jpeg")
        .header("Cache-Control", "public, no-cache")
        .header("Last-Modified", thumb.lastmod.to_rfc2822())
        .body(axum::body::Body::from(thumb.thumbnail))
        .context("thumb jpeg send make response")?
        .into_response();
    Ok(response)
}

/// Attempt to generate a thumbnail in JPEG format with hard-coded quality.
///
/// Because the generation can take a long time, it is delegated in a blocking
/// thread using Tokio.
#[instrument]
async fn gen_thumb<const TW: u32, const TH: u32>(
    userpathreal: &domainprim::ResolvedPath,
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

/// Serve the index page.
#[instrument]
async fn serve_index() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("index.html"))
}

/// Serve styles.css.
#[instrument]
async fn serve_styles() -> axum::response::Response {
    axum::response::Response::builder()
        .header("Content-Type", "text/css")
        .body(axum::body::Body::from(include_str!("styles.css")))
        .context("serve_styles: make response")
        .unwrap()
        .into_response()
}

/// Serve scripts.js
#[instrument]
async fn serve_scripts() -> axum::response::Response {
    axum::response::Response::builder()
        .header("Content-Type", "text/javascript")
        .body(axum::body::Body::from(include_str!("scripts.js")))
        .context("serve_scripts: make response")
        .unwrap()
        .into_response()
}

#[tokio::main]
#[instrument]
async fn main() {
    use tower_http::trace::TraceLayer;

    // Set up logging
    tracing_subscriber::fmt::init();

    // Before building app, ROOT must be set. It is the root directory
    // serving data.
    // First, check the arguments. We make a few assumptions.
    //  1. The first argument is the path to the executable.
    //  2. The second argument is the path to the root directory. <--- what we want.
    //  3. No glob patterns are used from the perspective of the program.
    //  (Unix/Linux shells typically expand them before passing them onto us.
    //   Windows shells typically don't expand them at all.)
    let args: Vec<String> = std::env::args().collect();
    let root = if args.len() < 2 {
        // Let the user know that the program expects a path to the root directory.
        // Still, we will use the current directory as the root directory.
        tracing::info!("No root directory specified. Using current directory. Usage: ./(program) (root directory)");
        std::env::current_dir().unwrap()
    } else {
        tracing::info!("Root directory specified: {arg:?}", arg = &args[1]);
        // TODO: Let the user know if the path uses glob patterns.
        let temp = PathBuf::from(&args[1]);
        tokio::fs::canonicalize(temp)
            .await
            .context("The root directory as specified failed to canonicalize")
            .unwrap()
    };
    tracing::info!("Serving at {root:?}");

    let root = domainprim::ResolvedPath::from_trusted_pathbuf(root);
    ROOT.set(root).unwrap();

    // Also, primitively cache the thumbnails.
    let cache_mpsc = cachethumb::spawn_cache_process();
    CACHEMPSC.set(cache_mpsc).unwrap();

    // Build app
    let app = Router::new()
        .route("/", get(|| async { Redirect::permanent("/user") }))
        .merge(
            // Static assets
            Router::new()
                .route("/user", get(serve_index))
                .route("/user/", get(serve_index))
                .route("/thumb", get(|| async { serve_svg(SVG_FILE).await }))
                .route("/thumb/", get(|| async { serve_svg(SVG_FILE).await }))
                .route(
                    "/thumbdir",
                    get(|| async { serve_svg(SVG_FOLDER).await }),
                )
                .route(
                    "/thumbdir/",
                    get(|| async { serve_svg(SVG_FOLDER).await }),
                )
                .route("/thumbimg", get(serve_loading_png))
                .route("/thumbimg/", get(serve_loading_png))
                .route("/styles.css", get(serve_styles))
                .route("/scripts.js", get(serve_scripts))
                // ... with HTTP caching
                .layer(map_response(add_static_cache_control)),
        )
        .merge(
            Router::new()
                .route("/root", get(serve_root))
                .route("/root/", get(serve_root))
                .route("/root/*userpath", get(serve_root))
                // Special route for dynamic thumbnails
                .route("/thumb/*userpath", get(serve_thumb::<_, 200, 200>))
                .layer(map_request(resolve_path)),
        )
        // basic logging
        .layer(TraceLayer::new_for_http());

    // Start server, listening on port 3000
    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
