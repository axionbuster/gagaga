//! File system
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

use anyhow::bail;
use async_stream::try_stream;
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

/// A label that signifies that some path buffer is relative to the
/// virtual root
pub type VirtualPath = Path;

/// A label that signifies that some path buffer is relative to the
/// computer (real root)
pub type RealPath = Path;

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

/// Convert a pair of the UTF-8 file name and native [Metadata](std::fs::Metadata)
/// to a [`FileMetadata`].
impl<S> TryFrom<(S, std::fs::Metadata)> for FileMetadata
where
    S: TryInto<String>,
{
    type Error = Error;

    fn try_from(
        value: (S, std::fs::Metadata),
    ) -> std::result::Result<Self, Self::Error> {
        let (fna, fme) = value;
        let fna = fna.try_into().map_err(|_| anyhow!("bad utf-8"))?;
        let fty = if fme.is_file() {
            FileType::RegularFile
        } else if fme.is_dir() {
            FileType::Directory
        } else if fme.file_type().is_symlink() {
            FileType::Link
        } else {
            bail!("unknown file type");
        };
        let lmo = fme.modified().map(|st| st.into()).ok();
        Ok(Self {
            file_type: fty,
            file_name: fna,
            size: fme.len(),
            last_modified: lmo,
        })
    }
}

/// A stream of FileMetadata's
pub type FileMetadataStream = Pin<Box<dyn Stream<Item = Result<FileMetadata>>>>;

/// Asynchronously list a directory
#[instrument]
pub async fn list_directory(
    chroot: impl AsRef<RealPath> + Debug + Send + Sync,
    virt_path: impl AsRef<VirtualPath> + Debug + Send + Sync,
) -> Result<FileMetadataStream> {
    let read_dir =
        tokio::fs::read_dir(chroot.as_ref().join(virt_path.as_ref()))
            .await
            .context("open read_dir")?;
    let read_dir = tokio_stream::wrappers::ReadDirStream::new(read_dir);
    fn make_stream(
        read_dir: tokio_stream::wrappers::ReadDirStream,
    ) -> impl Stream<Item = Result<FileMetadata>> {
        try_stream! {
            for await de in read_dir {
                // Find the file name
                let de = de
                    .context("get directory entry")?;
                let fna = de
                    .file_name()
                    .to_str()
                    .ok_or_else(|| anyhow!("file name bad utf-8"))?
                    .to_string();
                // Find the metadata
                let md = de.metadata().await.context("get metadata")?;
                // Go
                let md: FileMetadata = (fna, md).try_into()?;
                yield md;
            }
        }
    }
    let read_dir = make_stream(read_dir);
    Ok(Box::pin(read_dir))
}

/// Read the metadata of an individual file
#[instrument]
pub async fn read_metadata(
    chroot: impl AsRef<RealPath> + Debug + Send + Sync,
    virt_path: impl AsRef<VirtualPath> + Debug + Send + Sync,
) -> Result<FileMetadata> {
    // Find the file name
    let fna = virt_path
        .as_ref()
        .file_name()
        .ok_or_else(|| anyhow!("no file name"))?
        .to_str()
        .ok_or_else(|| anyhow!("bad utf-8"))?
        .to_string();

    // Get the metadata
    let md = tokio::fs::metadata(virt_path)
        .await
        .context("get metadata")?;

    // Convert the metadata to a FileMetadata
    Ok((fna, md).try_into()?)
}

/// Canonicalize a path by accessing the file system
#[instrument]
pub async fn canonicalize(
    chroot: impl AsRef<RealPath> + Debug + Send + Sync,
    virt_path: impl AsRef<VirtualPath> + Debug + Send + Sync,
) -> Result<PathBuf> {
    let real_path = chroot.as_ref().join(virt_path.as_ref());
    let real_path = tokio::fs::canonicalize(real_path).await?;
    Ok(real_path)
}