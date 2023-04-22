//! Thumbnail caching

use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::{
    prim::*,
    vfs::{ReadMetadata, VirtualPathBuf},
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
        srvpath: VirtualPathBuf,
        now: DateTime,
        dat: Vec<u8>,
    },
    /// Get only if fresh
    Get {
        srvpath: VirtualPathBuf,
        rpy: tokio::sync::oneshot::Sender<Option<CacheResponse>>,
    },
}

/// Caching Process
#[derive(Debug)]
pub struct CacheProcess(mpsc::UnboundedSender<Msg>);

impl CacheProcess {
    /// Permanently spawn a cache process.
    ///
    /// The lifetime of `vfs` is `'static` because of the restriction
    /// of `tokio::spawn`.
    pub fn spawn(vfs: impl ReadMetadata + 'static) -> Self {
        // Some channels
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            // Central data structure
            let mut hmp: HashMap<VirtualPathBuf, (DateTime, Vec<u8>)> =
                HashMap::new();
            tracing::info!("Cache process up");
            loop {
                match rx.recv().await {
                    Some(Msg::Ins { srvpath, now, dat }) => {
                        tracing::trace!("Inserting into cache");
                        hmp.insert(srvpath, (now, dat));
                    }
                    Some(Msg::Get { srvpath, rpy }) => {
                        tracing::trace!("Getting from cache");
                        let meta = vfs.read_metadata(&srvpath).await;
                        let fslmo = meta.ok().and_then(|m| m.last_modified);
                        let ca = hmp.get(&srvpath);
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
    pub fn ins(&self, srvpath: VirtualPathBuf, dat: Vec<u8>) {
        tracing::info!("Inserting into cache");
        let now = DateTime::now();
        let _ = self.0.send(Msg::Ins { srvpath, now, dat });
    }

    /// Attempt to get a thumbnail from the cache.
    ///
    /// It accesses the cache and the file system to determine whether
    /// the entry existed and whether it is fresh.
    ///
    /// NO GUARANTEES: This is a best-effort operation.
    pub async fn get(&self, srvpath: VirtualPathBuf) -> Option<Vec<u8>> {
        tracing::info!("Getting from cache");
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self.0.send(Msg::Get { srvpath, rpy: tx });
        rx.await.ok().flatten().map(|cr| cr.dat)
    }
}
