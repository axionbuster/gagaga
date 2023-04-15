//! VFS implementation A.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::primitive::{systime2datetime, Result};
use crate::vfs::{FileStat, FileType, VfsV1};

/// VFS implementation A.
#[derive(Debug, Clone, Copy)]
pub struct VfsImplA;

#[async_trait]
impl VfsV1 for VfsImplA {
    async fn canonicalize<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<PathBuf> {
        Ok(tokio::fs::canonicalize(path).await?)
    }

    fn canonicalizesync<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<PathBuf> {
        Ok(std::fs::canonicalize(path)?)
    }

    async fn stat<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<FileStat> {
        // Query the file system for the metadata.
        let meta = tokio::fs::metadata(path).await?;
        let lastmod = meta.modified().ok().and_then(systime2datetime);
        let size = meta.len();
        let file_type = if meta.is_file() {
            FileType::RegularFile
        } else if meta.is_dir() {
            FileType::Directory
        } else if meta.file_type().is_symlink() {
            FileType::Symlink
        } else {
            FileType::Unknown
        };

        Ok(FileStat::new(
            Some(path.as_ref().to_path_buf()),
            lastmod,
            size,
            file_type,
        ))
    }

    fn statsync<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
    ) -> Result<FileStat> {
        // Query the file system for the metadata.
        let meta = std::fs::metadata(path)?;
        let lastmod = meta.modified().ok().and_then(systime2datetime);
        let size = meta.len();
        let file_type = if meta.is_file() {
            FileType::RegularFile
        } else if meta.is_dir() {
            FileType::Directory
        } else if meta.file_type().is_symlink() {
            FileType::Symlink
        } else {
            FileType::Unknown
        };

        Ok(FileStat::new(
            Some(path.as_ref().to_path_buf()),
            lastmod,
            size,
            file_type,
        ))
    }

    fn listdirsync<P: AsRef<Path> + Send + Sync>(
        self,
        path: &P,
        limit: Option<usize>,
    ) -> Result<(bool, Vec<FileStat>)> {
        let mut n = 0;
        let mut entries = Vec::new();
        let mut dir = std::fs::read_dir(path)?;
        while limit.is_none() || n < limit.unwrap() {
            n += 1;
            let entry = dir.next();
            if entry.is_none() {
                break;
            }
            let entry = entry.unwrap();
            if entry.is_err() {
                continue;
            }
            let entry = entry.unwrap();
            let path = entry.path();
            let file_type = entry.file_type();
            if file_type.is_err() {
                continue;
            }
            let file_type = file_type.unwrap();
            let file_type = if file_type.is_dir() {
                FileType::Directory
            } else if file_type.is_file() {
                FileType::RegularFile
            } else if file_type.is_symlink() {
                FileType::Symlink
            } else {
                FileType::Unknown
            };
            let meta = entry.metadata();
            if meta.is_err() {
                continue;
            }
            let meta = meta.unwrap();
            let lastmod = meta.modified().ok().and_then(systime2datetime);
            let size = meta.len();
            let stat = FileStat::new(Some(path), lastmod, size, file_type);
            entries.push(stat);
        }
        let truncated =
            dir.next().is_some() || (limit.is_some() && n >= limit.unwrap());
        Ok((truncated, entries))
    }
}
