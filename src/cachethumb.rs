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
//! - The data structure, a [`HashMap`], is single threaded, which
//! requires the serialization of all instructions.
//! - I find it simpler than having to deal with concurrency
//! hazards myself in other places than this.
//! - It's concurrent anyway so it's hard.
//!
//! Example:
//! - Call [`spawn_cache_process()`] to get a channel to the cache manager.
//! It also spawns it!
//! - Compose [`CacheMessage::Insert`] to insert a new thumbnail.
//! Send it to the channel returned by the previous step.
//! - Compose [`CacheMessage::Get`] to get a thumbnail. Send it.
//!
//! For each one of these cases above, respectively, you can use:
//! - [`spawn_cache_process`]
//! - See the methods defined on [`Mpsc`], your primary point of
//!   interaction with the cache manager.

use tokio::sync::mpsc;
use tracing::instrument;

use crate::domain::RealPath;
use crate::primitive::*;
use crate::vfs::VfsV1;
use crate::vfsa::VfsImplA;

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
    /// Insert a new thumbnail (`Vec<u8>`) now.
    Insert(RealPath, Vec<u8>),
    /// Get a thumbnail (`Vec<u8>`) now only if fresh.
    ///
    /// The manager will inspect the file system asynchronously to
    /// determine freshness if necessary.
    Get(
        RealPath,
        tokio::sync::oneshot::Sender<Option<CacheResponse>>,
    ),
}

/// A channel to the cache manager "process" (logical).
///
/// Spawned by [`spawn_cache_process`].
///
/// This structure and its two methods are your primary point of
/// interaction with the cache manager.
///
/// You can use this structure to insert and get thumbnails.
///
/// All access to the cache is serialized. In fact, the event
/// loop is single threaded.
///
/// There's no guarantee that an insertion or a retrieval will
/// succeed even under normal circumstances. The message passing
/// system (Tokio) may drop the message for any number of
/// reasons, though it is convenient in this case.
///
/// If a message gets dropped (or the underlying system fails),
/// the cache manager may log it and continue. But it also may not
/// log it and continue. It's not guaranteed.
#[derive(Debug)]
pub struct Mpsc(mpsc::UnboundedSender<CacheMessage>);

impl Mpsc {
    /// Insert a new thumbnail (`Vec<u8>`) now.
    pub fn ins(&self, path: &RealPath, data: Vec<u8>) {
        self.0
            .send(CacheMessage::Insert(path.clone(), data))
            // If fail, quietly drop the message.
            .unwrap_or_default()
    }

    /// Get a thumbnail (`Vec<u8>`) now only if fresh.
    ///
    /// The manager will inspect the file system asynchronously to
    /// determine freshness if necessary.
    ///
    /// Note on the vocabulary:
    /// - Fresh: exists and cache is more recent than the file system.
    /// - Stale: exists but cache is older than the file system.
    /// - (Neither): missing, does not exist.
    pub async fn get(&self, path: &RealPath) -> Option<CacheResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.0.send(CacheMessage::Get(path.clone(), tx)).unwrap();
        // flatten: Option<Option<T>> -> Option<T>
        rx.await.ok().flatten()
    }
}

/// A cache manager "process" (logical). It's defined by an implicit
/// main loop, and it's not a real OS process. But whatever.
/// See [`Mpsc`] for how to communicate.
#[instrument]
pub fn spawn_cache_process() -> Mpsc {
    // Define the main loop and spawn it, too.
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        // Global data structure
        let mut cache: HashMap<RealPath, (DateTime, Vec<u8>)> = HashMap::new();
        // Event loop
        while let Some(msg) = rx.recv().await {
            match msg {
                CacheMessage::Insert(path, data) => {
                    tracing::trace!("Got insert");
                    cache.insert(path, (DateTime::now(), data));
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
                    let metadata = VfsImplA.stat(&path).await;
                    // For any I/O errors, just ignore it quietly.
                    if metadata.is_err() {
                        tracing::debug!("Get {path:?} was 'stale' (I/O error)");
                        reply_to.send(None).unwrap();
                        continue;
                    }
                    let metadata = metadata.unwrap();
                    let flastmod = metadata.lastmod();
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

                    // Decide.
                    if flastmod > clastmod {
                        // Stale
                        tracing::trace!("Get {path:?} was stale (fs > cache)");
                        reply_to.send(None).unwrap();
                    } else {
                        // Fresh
                        tracing::trace!("Get {path:?} was fresh");
                        reply_to
                            .send(Some(CacheResponse {
                                lastmod: clastmod.clone(),
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
