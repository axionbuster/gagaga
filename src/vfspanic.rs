//! An implementation of VFS that panics for all operations.

use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;

use crate::primitive::*;

use crate::vfs::*;

/// A VFS that panics for all operations.
#[deprecated(note = "This is only for testing purposes")]
#[derive(Debug, Clone, Copy)]
pub struct VfsPanic;

#[allow(deprecated)]
#[async_trait]
impl VfsV1 for VfsPanic {
    async fn canonicalize<P: AsRef<Path> + Send + Sync>(
        self,
        _path: &P,
    ) -> Result<PathBuf> {
        panic!("VfsPanic::canonicalize");
    }

    fn canonicalizesync<P: AsRef<Path> + Send + Sync>(
        self,
        _path: &P,
    ) -> Result<PathBuf> {
        panic!("VfsPanic::canonicalizesync");
    }

    async fn stat<P: AsRef<Path> + Send + Sync>(
        self,
        _path: &P,
    ) -> Result<FileStat> {
        panic!("VfsPanic::stat");
    }

    fn statsync<P: AsRef<Path> + Send + Sync>(
        self,
        _path: &P,
    ) -> Result<FileStat> {
        panic!("VfsPanic::statsync");
    }

    fn listdirsync<P: AsRef<Path> + Send + Sync>(
        self,
        _path: &P,
        _limit: Option<usize>,
    ) -> Result<(bool, Vec<FileStat>)> {
        panic!("VfsPanic::listdirsync");
    }

    async fn openfile<P: AsRef<Path> + Send + Sync>(
        self,
        _path: &P,
    ) -> Result<Box<dyn VfsTokioFile>> {
        panic!("VfsPanic::read_file");
    }
}
