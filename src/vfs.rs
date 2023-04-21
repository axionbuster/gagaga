//! Virtual File System
//!
//! This module specifies the virtual file system (VFS) of the server.
//!
//! It's suppose to be dumb and simple. It's not a full-fledged file system.

// Some commands

use std::ffi::OsStr;

use crate::primitive::{DateTime, Result};

// Reexport

// Don't know if it is a good idea to export
// tokio::io::AsyncReadExt and tokio::io::AsyncSeekExt.
pub use std::path::{Path, PathBuf};
pub use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt};

/// File type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// Unknown
    Unknown,
    /// Regular file
    RegularFile,
    /// Directory
    Directory,
    /// Symbolic link
    Symlink,
}

/// Combined metadata and directory entry
#[derive(Clone, Debug)]
pub struct FileStat {
    /// The absolute path of the file (unverified)
    path: Option<PathBuf>,
    /// Last Modified
    lastmod: Option<DateTime>,
    /// File size, bytes
    size: u64,
    /// Indicate type of file object
    file_type: FileType,
}

impl FileStat {
    /// Create a new FileStat
    pub fn new(
        path: Option<PathBuf>,
        lastmod: Option<DateTime>,
        size: u64,
        file_type: FileType,
    ) -> Self {
        Self {
            path,
            lastmod,
            size,
            file_type,
        }
    }

    /// Get the absolute path of the file (unverified)
    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    /// Get the last modified time, if it exists
    pub fn lastmod(&self) -> Option<&DateTime> {
        self.lastmod.as_ref()
    }

    /// Get the base name of the file as OsStr
    pub fn basename(&self) -> Option<&OsStr> {
        self.path.as_ref().and_then(|p| p.file_name())
    }

    /// Get the file size, if known
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Decide if regular file. Don't follow symlinks.
    pub fn isfile(&self) -> bool {
        self.file_type == FileType::RegularFile
    }

    /// Decide if directory. Don't follow symlinks.
    pub fn isdir(&self) -> bool {
        self.file_type == FileType::Directory
    }

    /// Decide if symlink.
    pub fn issymlink(&self) -> bool {
        self.file_type == FileType::Symlink
    }
}

/// A handle to a file.
pub trait VfsTokioFile: AsyncRead + AsyncSeek + Unpin + Send {}

/// A file opened by Tokio. Implements AsyncRead + AsyncSeek.
pub struct TokioFile(tokio::fs::File);

impl AsyncRead for TokioFile {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        tokio::io::AsyncRead::poll_read(
            std::pin::Pin::new(&mut self.get_mut().0),
            cx,
            buf,
        )
    }
}

impl AsyncSeek for TokioFile {
    fn start_seek(
        self: std::pin::Pin<&mut Self>,
        pos: std::io::SeekFrom,
    ) -> std::result::Result<(), std::io::Error> {
        tokio::io::AsyncSeek::start_seek(
            std::pin::Pin::new(&mut self.get_mut().0),
            pos,
        )
    }

    fn poll_complete(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<u64>> {
        tokio::io::AsyncSeek::poll_complete(
            std::pin::Pin::new(&mut self.get_mut().0),
            cx,
        )
    }
}

impl VfsTokioFile for TokioFile {}

impl From<tokio::fs::File> for TokioFile {
    fn from(file: tokio::fs::File) -> Self {
        Self(file)
    }
}

/// A specification of a virtual file system.
///
/// Implementations are expected be zero-sized structs.
///
/// - Open and read or seek into files asynchronously. See
/// [`AsyncRead`](tokio::io::AsyncRead) and [`AsyncSeek`](tokio::io::AsyncSeek).
/// - Get metadata of objects synchronously or asynchronously.
/// - List directory contents synchronously (TODO: asynchronously).
/// - Canonicalize paths synchronously or asynchronously.
#[async_trait::async_trait]
pub trait VfsV1: Copy + Send + Sync + 'static {
    /// Asynchronously canonicalize a path.
    async fn canonicalize<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<PathBuf>;

    /// Canonicalize a path, synchronously.
    fn canonicalizesync<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<PathBuf>;

    /// Asynchronously get the metadata of a file.
    async fn stat<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<FileStat>;

    /// Synchronously get the metadata of a file.
    fn statsync<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<FileStat>;

    /// List the contents of a directory with an optional number of files limit.
    ///
    /// I/O errors will be silently ignored.
    ///
    /// It also indicates whether the result is truncated.
    ///
    /// Currently, this function is synchronous.
    fn listdirsync<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
        limit: Option<usize>,
    ) -> Result<(bool, Vec<FileStat>)>;

    /// Open a file to start reading asynchronously.
    async fn openfile<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<Box<dyn VfsTokioFile>> {
        Ok(Box::new(TokioFile::from(
            tokio::fs::File::open(path).await?,
        )))
    }
}
