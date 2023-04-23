//! Thumbnail caching

use std::{collections::HashMap, fmt::Debug, path::PathBuf};

use tokio::sync::mpsc;

use crate::{
    fs::{read_metadata, RealPath},
    prim::*,
};

/// Inspect the response from the cache process
#[derive(Debug)]
pub struct CacheResponse {
    /// The thumbnail data
    pub dat: Vec<u8>,
    /// Last modified time
    pub lmo: DateTime,
}

/// An internal message
#[derive(Debug)]
enum Msg {
    /// Insert Now
    Ins {
        virt_path: PathBuf,
        now: DateTime,
        dat: Vec<u8>,
    },
    /// Get only if fresh
    Get {
        virt_path: PathBuf,
        rpy: tokio::sync::oneshot::Sender<Option<CacheResponse>>,
    },
}

/// Caching Process
#[derive(Debug)]
pub struct CacheProcess(mpsc::UnboundedSender<Msg>);

impl CacheProcess {
    /// Permanently spawn a cache process.
    pub fn spawn(
        chroot: impl AsRef<RealPath> + Send + Sync + Debug + 'static,
    ) -> Self {
        // Some channels
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            // Central data structure
            let mut hmp: HashMap<PathBuf, (DateTime, Vec<u8>)> = HashMap::new();
            tracing::info!("Cache process up");
            loop {
                match rx.recv().await {
                    Some(Msg::Ins {
                        virt_path,
                        now,
                        dat,
                    }) => {
                        tracing::trace!("Inserting into cache");
                        hmp.insert(virt_path, (now, dat));
                    }
                    Some(Msg::Get { virt_path, rpy }) => {
                        tracing::trace!("Getting from cache");
                        let meta = read_metadata(&chroot, &virt_path).await;
                        let fslmo = meta.ok().and_then(|m| m.last_modified);
                        let ca = hmp.get(&virt_path);
                        let fresh = fslmo.is_some()
                            && ca.is_some()
                            && fslmo.unwrap() < ca.unwrap().0;
                        if fresh {
                            tracing::trace!("Cache hit");
                            let (lmo, dat) = ca.unwrap().clone();
                            let _ = rpy.send(Some(CacheResponse { dat, lmo }));
                        } else {
                            tracing::trace!("Cache miss");
                            let _ = rpy.send(None);
                        }
                    }
                    None => {
                        tracing::info!("Cache process shutting down");
                        return;
                    }
                }
            }
        });
        Self(tx)
    }

    /// Attempt to insert a thumbnail into the cache.
    ///
    /// It may succeed or fail but it will not block.
    ///
    /// NO GUARANTEES: The cache may be full or the cache process may
    /// have shut down. And, even when the cache is in good state,
    /// the sending may fail for other reasons.
    pub fn ins(&self, virt_path: PathBuf, dat: Vec<u8>) {
        tracing::info!("Inserting into cache");
        let now = DateTime::now();
        let _ = self.0.send(Msg::Ins {
            virt_path,
            now,
            dat,
        });
    }

    /// Attempt to get a thumbnail from the cache.
    ///
    /// It accesses the cache and the file system to determine whether
    /// the entry existed and whether it is fresh.
    ///
    /// NO GUARANTEES: This is a best-effort operation.
    pub async fn get(&self, virt_path: PathBuf) -> Option<Vec<u8>> {
        tracing::info!("Getting from cache");
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self.0.send(Msg::Get { virt_path, rpy: tx });
        rx.await.ok().flatten().map(|cr| cr.dat)
    }
}
