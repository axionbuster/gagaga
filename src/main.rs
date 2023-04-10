use std::path::PathBuf;

use axum::{response::Html, routing::get, Router};
use http::StatusCode;

mod domainprim {
    //! Define domain-specific types and processes
    use std::path::{Path, PathBuf};

    use anyhow::Context;
    use chrono::TimeZone;
    use serde::Serialize;

    /// UTC DateTime
    type DateTime = chrono::DateTime<chrono::Utc>;

    /// Attempt to convert a SystemTime (returned on file statistics calls)
    /// to the DateTime type. How inconvenient is this?
    pub fn systime2datetime(t: std::time::SystemTime) -> Option<DateTime> {
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| chrono::Utc.timestamp_opt(d.as_secs() as i64, d.subsec_nanos()))
            .map(|t| t.single().unwrap())
    }

    /// An absolute, resolved file path that was trusted when it was created.
    /// This is relative to the server's computer.
    #[derive(Debug, Clone)]
    pub struct ResolvedPath(PathBuf);

    /// Expose the &Path reference
    impl AsRef<Path> for ResolvedPath {
        fn as_ref(&self) -> &Path {
            self.0.as_ref()
        }
    }

    /// Errors in resolving a path
    #[derive(Debug, thiserror::Error)]
    pub enum ResolvePathError {
        #[error("Path {path:?} is not a subpath of parent {parent:?}")]
        NotSubpath { path: PathBuf, parent: PathBuf },

        #[error("Any I/O error")]
        IoError(
            #[from]
            #[source]
            std::io::Error,
        ),

        #[error("Any other error")]
        OtherError(
            #[from]
            #[source]
            anyhow::Error,
        ),
    }

    /// Attempt to resolve a path asynchronously and admit it if
    /// it is a subpath of the right path, an absolute, similarly
    /// resolved path.
    pub async fn pathresolve(
        path: &Path,
        parent: &ResolvedPath,
    ) -> Result<ResolvedPath, ResolvePathError> {
        // Ask Tokio to resolve the path asynchronously
        let path: Result<PathBuf, std::io::Error> = tokio::fs::canonicalize(path).await;
        let path: PathBuf = path?;

        // Decide whether the resolved path is a subpath of the parent
        if !path.starts_with(parent.as_ref()) {
            // anyhow::bail!("Path {path:?} is not a subpath of parent {parent:?}");
            return Err(ResolvePathError::NotSubpath {
                path,
                parent: parent.as_ref().to_path_buf(),
            });
        }

        Ok(ResolvedPath(path))
    }

    /// Just admit a PathBuf as a ResolvedPath
    pub fn admitpathbuf(path: PathBuf) -> ResolvedPath {
        ResolvedPath(path)
    }

    /// A regular file or directory.
    pub struct DomainFile {
        /// The path to the file or directory.
        pub path: ResolvedPath,
        /// The last modified time of the file or directory.
        pub last_modified: Option<DateTime>,
        /// The size of the file or directory.
        pub size_bytes: u64,
    }

    impl DomainFile {
        /// Extract the final component of the path.
        /// If can't, then return an empty string.
        pub fn display_name(&self) -> String {
            self.path
                .as_ref()
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "".to_string())
        }

        /// Extract the URL to expose to the user from the root directory
        /// of the server.
        pub fn display_url(&self, serve_root: &ResolvedPath) -> Option<String> {
            // Use a trim to cut out the longest
            // string match between the base and the path starting at
            // path[0].
            let path = self.path.as_ref();
            let serve_root = serve_root.as_ref();
            let path = path.strip_prefix(serve_root);

            // If the path is not a subpath of the serve root, then
            // something is wrong.
            let _dbg_path_clone = path.clone();
            let path = path
                .with_context(|| {
                    format!(
                        r#"display_url: Path {:?} is not a subpath of serve root {:?}.
Something is wrong. serve root should have been set by the program and is
not user-controllable, or maybe not?"#,
                        _dbg_path_clone, serve_root
                    )
                })
                .unwrap();
            let path = path.to_str()?;

            // If the path is empty, then we're at the root directory.
            // Return the root URL.
            if path.is_empty() {
                return Some("/root".to_string());
            }

            // Layout: "/root/path"
            let mut url = String::new();
            url.push_str("/root");
            if !path.starts_with('/') {
                url.push('/');
            }
            url.push_str(path);
            Some(url)
        }

        // FIXME: Locale support is way too hairy for me to deal with right now.
        // I'm going to have to come back to this later.

        /// Calculate the size of the file or directory in a human-readable
        /// format for English speakers living in the US.
        pub fn display_size_en_us(&self) -> String {
            // Simply the number of bytes.
            // Client JavaScript expects this format.
            format!("{}", self.size_bytes)
        }

        /// Calculate the last modified time of the file or directory in a
        /// human-readable format for English speakers living in the US.
        pub fn display_last_modified_en_us(&self) -> String {
            // Similarly, just use something that works for now.
            if let Some(last_modified) = self.last_modified {
                // RFC 3339, ISO 8601 date and time format
                // Client JavaScript expects this format.
                last_modified.to_rfc3339()
            } else {
                "".to_string()
            }
        }
    }

    // We need some custom serialization for DomainFile
    // because client JavaScript expects a specific format.
    impl Serialize for DomainFile {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            use serde::ser::SerializeMap;
            let mut state = serializer.serialize_map(None)?;
            state.serialize_entry("display_name", &self.display_name())?;
            state.serialize_entry(
                "url",
                &self.display_url(&admitpathbuf(std::env::current_dir().unwrap())),
            )?;
            state.serialize_entry("size_bytes", &self.display_size_en_us())?;
            state.serialize_entry("last_modified", &self.display_last_modified_en_us())?;
            state.end()
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
        dirs: &mut Vec<DomainFile>,
        files: &mut Vec<DomainFile>,
        limit_reached: &mut bool,
    ) -> anyhow::Result<()> {
        // Walk the directory
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
            // Call fstat or something like that.
            let entry = entry.unwrap();
            let metadata = entry.metadata().await?;
            // Sometimes there's stuff like symlinks and all that that
            // kinda goes against what we can allow the user to see.
            // Bounce them quietly.
            let path = entry.path();
            let path = pathresolve(&path, parent_path).await;
            if path.is_err() {
                continue;
            }
            let path: ResolvedPath = path?;
            let domainfile = DomainFile {
                path,
                last_modified: metadata.modified().ok().and_then(systime2datetime),
                size_bytes: metadata.len(),
            };

            // Decide whether the entry is a directory or a file
            if metadata.is_dir() {
                // It's a directory!
                dirs.push(domainfile);
            } else if metadata.is_file() {
                // Now, it's a file!
                files.push(domainfile);
            } else {
                // Do nothing if it's neither a file nor a directory.
                // Stuff like devices.
            }
        }

        // If the limit is reached, then set the flag
        if n == N {
            *limit_reached = true;
        }

        Ok(())
    }

    /// By looking at the extension of a &Path only, heuristically decide whether
    /// the file might be one of the thumbnail-supported JPEG file.
    /// I will refactor this code to support more than just JPEG. But, for now,
    /// I'm going to delegate the flexibility to the human programmer.
    pub fn extjpeg(path: &Path) -> bool {
        // Check if the extension is jpeg or jpg
        let ext = path
            .extension()
            .and_then(|ext: &std::ffi::OsStr| ext.to_str())
            .unwrap_or("");

        // .to_ascii_lowercase() allocates, so I want to avoid that for
        // most cases.
        matches!(ext, "jpeg" | "jpg" | "JPEG" | "JPG")
            || matches!(ext.to_ascii_lowercase().as_str(), "jpeg" | "jpg")
    }

    /// In-memory representation of an image file.
    pub struct MemImg(image::DynamicImage);

    /// Attempt to perform the compound action of accepting a
    /// resolved and trusted path,
    /// opening it, and reading it into memory,
    /// and then, lastly, determining its thumbnail.
    ///
    /// Obviously, it's due to a refactoring. It's because I'm
    /// going to add a caching layer and more formats.
    pub fn genthumb<const TW: u32, const TH: u32>(path: &ResolvedPath) -> anyhow::Result<MemImg> {
        // Use image crate, open and read image.
        let dynimg: image::DynamicImage = image::io::Reader::open(path.as_ref())?.decode()?;

        // Generate thumbnail
        let dynimg = dynimg.thumbnail(TW, TH);

        // Cool!
        Ok(MemImg(dynimg))
    }
}

/// Some HTTP-related errors.
#[derive(Debug, thiserror::Error)]
enum MyError {
    #[error("Not Found")]
    NotFound,

    // I didn't know this, but if you don't have these,
    // then you can't use the question-mark operator (?)
    // to propagate errors. Also, if you don't use BOTH
    // #[from] and #[source], then you still can't use
    // the operator (?) because the propagation works by
    // implicitly converting the errors using the From trait,
    // and From<(whatever Error)> won't
    // be implemented until both are used.
    // ^ note to self.
    /// Anyhow error
    #[error("Internal Server Error, A")]
    AnyhowError(
        #[from]
        #[source]
        anyhow::Error,
    ),

    /// IO error
    #[error("Internal Server Error, I")]
    IoError(
        #[from]
        #[source]
        std::io::Error,
    ),

    /// Sailfish (template engine) error
    #[error("Internal Server Error, S")]
    SailfishRenderError(
        #[from]
        #[source]
        sailfish::RenderError,
    ),
}

/// Serve a file or directory, downloading if a regular file,
/// or listing if a directory.
async fn serve_user_path_core(
    userpath: axum::extract::Path<String>,
) -> Result<Html<String>, MyError> {
    // Domain-specific primitives
    use crate::domainprim::{admitpathbuf, dirlist, pathresolve, DomainFile, ResolvedPath};

    // What's up, user. How are you doing?
    let userpath: String = userpath.0;
    let userpath: PathBuf = PathBuf::from(userpath);

    // Executable's directory. Will refactor to consider other places
    // than just the place where the executable is.
    let rootdir: PathBuf = std::env::current_dir()?;
    let rootdir: ResolvedPath = admitpathbuf(rootdir);

    // Resolve the path (convert user's path to server's absolute path, as well as
    // following symlinks and all that). Note: according to the contract of
    // ResolvedPath, it's guaranteed to be absolute and within the root directory.
    let userpathreal = pathresolve(&userpath, &rootdir).await;
    if let Err(e) = &userpathreal {
        use crate::domainprim::ResolvePathError;
        match e {
            // If the stripping didn't work, then it's a 404.
            // If the file also wasn't found, then it's a 404.
            ResolvePathError::NotSubpath { path: _, parent: _ } => return Err(MyError::NotFound),
            ResolvePathError::IoError(_) => return Err(MyError::NotFound),
            e => return Err(anyhow::anyhow!("unhandled error: {e}").into()),
        }
    }
    let userpathreal: ResolvedPath = userpathreal.unwrap();

    // Check if the path is a directory or a file, setting the flags.
    let mut reg = false;
    let mut dir = false;
    let filemetadata = userpathreal.as_ref().metadata()?;
    if filemetadata.is_dir() {
        dir = true;
    } else if filemetadata.is_file() {
        reg = true;
    }

    // If it's a regular file, then download it.
    if reg {
        return Err(MyError::AnyhowError(anyhow::anyhow!(
            "download not implemented"
        )));
    }

    // If it's not a directory, then say, not found.
    if !dir {
        return Err(MyError::NotFound);
    }

    // A directory. List it.
    // Start by setting aside some space in memory for
    // the files and sub-directories.
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut limit_reached = false;

    // List the directory with a limit of 3000 files.
    dirlist::<3000>(
        // path (user's control)
        &userpathreal,
        // don't go outside of the root directory (server's control)
        &rootdir,
        // collect regular files here
        &mut files,
        // and collect directories here
        &mut directories,
        // lastly, set this flag if the limit is reached
        &mut limit_reached,
    )
    .await?;

    // Generate HTML
    use sailfish::TemplateOnce;

    /// My template for a directory listing.
    #[derive(TemplateOnce)]
    #[template(path = "directory.stpl")]
    struct DirectoryTemplate<'a> {
        files: &'a Vec<DomainFile>,
        directories: &'a Vec<DomainFile>,
        limit_reached: bool,
    }

    // Generate HTML
    let instance = DirectoryTemplate {
        files: &files,
        directories: &directories,
        limit_reached,
    };
    let html = instance.render_once()?;

    // Go
    Ok(Html(html))
}

async fn serve_user_path(userpath: axum::extract::Path<String>) -> (StatusCode, Html<String>) {
    let reply = serve_user_path_core(userpath).await;

    // If there's a not found error, then return a 404.
    if let Err(MyError::NotFound) = reply {
        return (StatusCode::NOT_FOUND, Html(
            "<html><head><title>404 Not Found</title></head><body><h1>404 Not Found</h1></body></html>"
                .to_string(),
        ));
    }

    // If there's any other error, then return a 500.
    if reply.is_err() {
        // Log it
        tracing::error!("error: {:#?}", reply);

        return (StatusCode::INTERNAL_SERVER_ERROR, Html(
            "<html><head><title>500 Internal Server Error</title></head><body><h1>500 Internal Server Error</h1></body></html>"
                .to_string(),
        ));
    }

    // If there's no error, then return the HTML.
    (StatusCode::OK, reply.unwrap())
}

#[tokio::main]
async fn main() {
    // Set up logging
    tracing_subscriber::fmt::init();

    // Build app
    let app = Router::new()
        .route(
            "/root",
            get(|| async { serve_user_path(axum::extract::Path("./".to_string())).await }),
        )
        .route("/root/*userpath", get(serve_user_path));

    // Start server, listening on port 3000
    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
