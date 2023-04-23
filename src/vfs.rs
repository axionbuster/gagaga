//! Virtual file system
//!
//! Currently, handles the following:
//! - Defining an object as one of file, directory, link or unknown
//! - Defining the metadata for an object
//! - Listing files in a directory as an asynchronous stream

use std::{
    fmt::Debug,
    path::{Component, Path, PathBuf},
    pin::Pin,
};

use async_stream::stream;
use async_trait::async_trait;
use tokio::fs::DirEntry;
use tokio_stream::Stream;

use crate::prim::*;

/// Determine whether a file path has prohibited characters
/// or other restricted parts.
///
/// Regardless of the platform, the following constructs are
/// prohibited:
/// - Longer than 2,048 bytes (depends on encoding)
/// - Invalid UTF-8
/// - ASCII control characters
/// - `/ < > : " / \ | ? *`
/// - `CON PRN AUX NUL COM[1-9] LPT[1-9]` by themselves or by
/// themselves before the extension
/// - Non-normal paths (such as `..`, `.` or `//`)
///
/// It's possible that the longest path on Windows that is
/// admitted by this algorithm is significantly shorter than
/// what is admitted under Unix-like platforms due to the encoding.
#[instrument(skip(p), fields(osstrlen = p.as_ref().as_os_str().len()))]
pub fn bad_path1(p: impl AsRef<Path> + Debug) -> bool {
    // Long
    // Note: .len() does NOT refer to the number of bytes in the
    // path, but how many were in memory. If you only compile for
    // Unix-like platforms, you could use .as_bytes().len() instead,
    // (.as_bytes() being defined on Unix-like platforms only),
    // but that wouldn't work on Windows.
    if p.as_ref().as_os_str().len() > 2048 {
        tracing::trace!("Path too long, reject");
        return true;
    }

    // Invalid UTF-8 check. Also get a UTF-8 representation.
    let sp = p.as_ref().to_str();
    if sp.is_none() {
        tracing::trace!(
            "Path not valid UTF-8, reject. \
Best rendering (with escapes): {render:?}",
            render = p.as_ref().to_string_lossy()
        );
        return true;
    }
    let sp = sp.unwrap();

    // Control characters or Windows-specific bad characters, but
    // enforced for all platforms anyway
    let ctrl = sp.matches(|c: char| {
        c.is_ascii_control()
            || matches!(c, '/' | '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*')
    });
    if let Some(c) = ctrl.into_iter().next() {
        tracing::trace!(
            "Path contains a bad character ({c:?}), reject. Path: {sp:?}"
        );
        return true;
    }

    // Some prohibited (Windows) file names.
    // (Again, this is enforced for all platforms.)
    for component in p.as_ref().components() {
        if let Component::Normal(component) = component {
            let component2 = component.to_str();
            if component2.is_none() {
                // This is a highly unusual situation that should be
                // alerted to the user. Crafted string?

                // On Windows, produce a Vec<u16>.
                // On other Unix or WASI, produce a Vec<u8>.
                // Other platforms are not supported.
                #[cfg(windows)]
                let breakdown =
                    std::os::windows::prelude::OsStrExt::encode_wide(component)
                        .collect::<Vec<_>>();
                #[cfg(not(windows))]
                let breakdown =
                    std::os::unix::prelude::OsStrExt::as_bytes(component)
                        .to_vec();

                tracing::warn!(
                    "Path component ({component:?}) not UTF-8, \
though whole path ({sp:?}) is UTF-8. \
(utf8-len: {len} bytes). Reject.",
                    len = breakdown.len()
                );
                return true;
            }
            let component = component2.unwrap();

            // Strip anything after the first period (.)
            let component = if let Some((x, _)) = component.split_once('.') {
                x
            } else {
                component
            };

            // Detect leading or trailing whitespace in component
            // If exists, log the bad character (`bad`) and reject
            let mut bad = '\0';
            let bad_start = component.starts_with(|c: char| {
                bad = c;
                c.is_whitespace()
            });
            let has_bad = bad_start
                || component.ends_with(|c: char| {
                    bad = c;
                    c.is_whitespace()
                });
            if has_bad {
                tracing::trace!("Path component has leading or trailing whitespace ({bad:?}), reject. \
Component: {component:?}");
                return true;
            }
            let component = component.trim();

            if matches!(component, "CON" | "PRN" | "AUX" | "NUL") {
                tracing::trace!("Path component is a reserved name, reject. Component: {component:?}");
                return true;
            }

            if matches!(&component.get(..=3), Some("COM" | "LPT")) {
                // A single digit
                let c = component.get(4..).and_then(|s| s.chars().next());
                if c.is_none() {
                    continue;
                }
                let c = c.unwrap();
                if c.is_ascii_digit() {
                    tracing::trace!("Path component is a reserved name, reject. Component: {component:?}");
                    return true;
                }
            }
        } else {
            // Not a normal component
            tracing::trace!("Non-normal component in path, reject. Component: {component:?}");
            return true;
        }
    }

    // Passed everything with flying colors
    false
}

/// Define a file type
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileType {
    RegularFile,
    Directory,
    Link,
}

/// A path relative to the VFS root.
///
/// Unless stated otherwise, it's not guaranteed that the path
/// is absolute, relative, valid, normal, etc.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtualPathBuf(pub PathBuf);

impl AsRef<Path> for VirtualPathBuf {
    fn as_ref(&self) -> &Path {
        self.0.as_ref()
    }
}

impl From<PathBuf> for VirtualPathBuf {
    fn from(p: PathBuf) -> Self {
        Self(p)
    }
}

/// Metadata for a file object
#[non_exhaustive]
pub struct FileMetadata {
    /// Type of file
    pub file_type: FileType,
    /// File name (not path)
    pub file_name: String,
    /// File size, bytes
    pub size: u64,
    /// Last modified
    pub last_modified: Option<DateTime>,
}

/// A stream of FileMetadata's
pub type FileMetadataStream = Pin<Box<dyn Stream<Item = Result<FileMetadata>>>>;

/// Listing function
#[async_trait]
pub trait ListDirectory: Send + Sync {
    /// List a directory from a path relative to the VFS root
    async fn list_directory(
        &self,
        virt_path: impl AsRef<Path> + Debug + Send + Sync,
    ) -> Result<FileMetadataStream>;
}

/// A Tokio implementation
#[derive(Debug)]
pub struct TokioBacked {
    /// Root of the filesystem in the real world, absolute path
    pub real_root: PathBuf,
}

#[async_trait]
impl ListDirectory for TokioBacked {
    #[instrument]
    async fn list_directory(
        &self,
        virt_path: impl AsRef<Path> + Debug + Send + Sync,
    ) -> Result<FileMetadataStream> {
        let read_dir =
            tokio::fs::read_dir(self.real_root.join(virt_path.as_ref()))
                .await?;
        let read_dir = tokio_stream::wrappers::ReadDirStream::new(read_dir);
        let read_dir = map1(read_dir);
        let read_dir = map2(read_dir);
        Ok(Box::pin(read_dir))
    }
}

/// Convert a stream of [`DirEntry`]'s to ([`String`] \[filename\], [`std::fs::Metadata`])
#[instrument(skip(stream))]
fn map1<S: Stream<Item = std::io::Result<DirEntry>>>(
    stream: S,
) -> impl Stream<Item = Result<(String, std::fs::Metadata)>> {
    stream! {
        for await de in stream {
            let de = de?;
            let md = de.metadata().await?;
            let fna = de.file_name();
            let fna = fna.to_str().ok_or_else(|| anyhow::anyhow!("filename {fna:?} not valid Unicode"));
            let yil = fna.map(|fna| (fna.to_string(), md)).map_err(Error::IO);
            yield yil;
        }
    }
}

/// Convert a stream of ([`String`] \[filename\], [`std::fs::Metadata`]) to [`FileMetadata`]
#[instrument(skip(stream))]
fn map2<S: Stream<Item = Result<(String, std::fs::Metadata)>>>(
    stream: S,
) -> impl Stream<Item = Result<FileMetadata>> {
    stream! {
        for await md in stream {
            if let Err(e) = md {
                yield Err(e);
                continue;
            }
            let md = md.unwrap();

            let (fna, md) = md;
            let fty = md.file_type();
            let fty = if fty.is_file() {
                FileType::RegularFile
            } else if fty.is_dir() {
                FileType::Directory
            } else if fty.is_symlink() {
                FileType::Link
            } else {
                yield Err(Error::IO(anyhow::anyhow!("Unknown file type")));
                continue;
            };
            let lmo = md.modified().ok().map(DateTime::from);

            yield Ok(FileMetadata {
                file_type: fty,
                file_name: fna,
                size: md.len(),
                last_modified: lmo
            });
        }
    }
}

/// VFS functionality that reads files' metadata
#[async_trait]
pub trait ReadMetadata: Send + Sync {
    /// Read metadata for a file
    async fn read_metadata(
        &self,
        virt_path: impl AsRef<Path> + Debug + Send + Sync + Clone,
    ) -> Result<FileMetadata>;
}

#[async_trait]
impl ReadMetadata for TokioBacked {
    #[instrument]
    async fn read_metadata(
        &self,
        virt_path: impl AsRef<Path> + Debug + Send + Sync + Clone,
    ) -> Result<FileMetadata> {
        let vpa = virt_path.clone();
        let fna = vpa
            .as_ref()
            .file_name()
            .ok_or_else(|| Error::IO(anyhow::anyhow!("no file name")))?;
        let fna = fna.to_str().ok_or_else(|| {
            Error::IO(anyhow::anyhow!("filename {fna:?} not valid Unicode"))
        })?;
        let md = tokio::fs::metadata(virt_path).await.map_err(|e| {
            Error::IO(anyhow::anyhow!("get metadata {fna:?}: {e}"))
        })?;
        let fty = md.file_type();
        let fty = if fty.is_file() {
            FileType::RegularFile
        } else if fty.is_dir() {
            FileType::Directory
        } else if fty.is_symlink() {
            FileType::Link
        } else {
            return Err(Error::IO(anyhow::anyhow!("Unknown file type")));
        };
        let lmo = md.modified().ok().map(DateTime::from);

        Ok(FileMetadata {
            file_type: fty,
            file_name: fna.to_string(),
            size: md.len(),
            last_modified: lmo,
        })
    }
}

/// VFS functionality that canonicalizes a path by accessing it in
/// the real filesystem
#[async_trait]
pub trait Canonicalize {
    /// Canonicalize a path stated relative to the VFS root
    async fn canonicalize(
        &self,
        virt_path: impl AsRef<Path> + Debug + Send + Sync,
    ) -> Result<PathBuf>;
}

#[async_trait]
impl Canonicalize for TokioBacked {
    async fn canonicalize(
        &self,
        virt_path: impl AsRef<Path> + Debug + Send + Sync,
    ) -> Result<PathBuf> {
        let real_path = self.real_root.join(virt_path.as_ref());
        let real_path = tokio::fs::canonicalize(real_path).await?;
        Ok(real_path)
    }
}
