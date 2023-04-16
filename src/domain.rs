//! Define domain-specific types and processes
use std::os::unix::prelude::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use rand::seq::SliceRandom;
use serde_json::{json, Value};
use tracing::instrument;

use crate::primitive::*;

use crate::vfs;

/// An absolute, resolved file path that was trusted when it was created.
/// This is relative to the server's computer. This path is "real,"
/// meaning it's not virtualized.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RealPath(PathBuf);

impl RealPath {
    /// Trust a PathBuf with no validation whatsoever.
    ///
    /// This should be used only when the path is known to be
    /// absolute and resolved, and is not user input.
    ///
    /// Hence, no [`From`] implementation is provided.
    ///
    /// It should be a valid, absolute path that cannot be further
    /// resolved.
    pub fn from_trusted_pathbuf(path: PathBuf) -> RealPath {
        RealPath(path)
    }
}

/// Expose the &Path reference
impl AsRef<Path> for RealPath {
    fn as_ref(&self) -> &Path {
        self.0.as_ref()
    }
}

/// Given a normal, relative path (to the given working directory),
/// attempt to canonicalize it by following symlinks and resolving
/// paths, producing an absolute path.
#[instrument(err, level = "debug", skip(vfs))]
pub async fn pathresolve(
    pathuser: &Path,
    workdir: &RealPath,
    vfs: impl vfs::VfsV1,
) -> Result<RealPath> {
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
    let path = vfs.canonicalize(&meantpath).await?;

    // Decide whether the resolved path is a subpath of the parent
    if !path.starts_with(workdir.as_ref()) {
        return Err(UnifiedError::NotFound(anyhow::anyhow!(
            "Path {path:?} is not a subpath of the working \
directory {workdir:?}"
        )));
    }

    Ok(RealPath(path))
}

/// Produce a JSON response from the directory listing that can be
/// consumed by the frontend.
#[instrument(err, level = "debug", skip(vfs))]
pub async fn dirlistjson<const N: usize>(
    path: &RealPath,
    parent_path: &RealPath,
    vfs: impl vfs::VfsV1,
) -> Result<Value> {
    let path = path.clone();
    let parent_path = parent_path.clone();
    tokio::task::spawn_blocking(move || {
        // Files and directories are collected separately, then shuffled.
        let mut files = Vec::new();
        let mut directories = Vec::new();

        // Collect entries
        let path = &path;
        let (truncated, entries) = vfs.listdirsync(path, Some(N))?;
        for entry in entries {
            let name = entry.basename();
            if name.is_none() {
                continue;
            }
            let is_symlink = entry.issymlink();

            // If symlink, that of the resolved target.
            // Under this interpretation, it's definitely possible to observe
            // ("is_symlink" AND ("is_dir" OR "is_file")) == true.
            let is_dir;
            let is_file;

            // If symlink, probe the target
            if is_symlink {
                tracing::trace!("dirlistjson: Symlink: {:#?}", entry);
                // Resolve the target.
                // If it's not a file or directory, skip.
                let target = vfs.canonicalizesync(entry.path().unwrap());
                if target.is_err() {
                    tracing::trace!("dirlistjson: Resolve symlink fail: {:#?}", entry);
                    continue;
                }
                let target = target.unwrap();
                let metadata = vfs.statsync(&target);
                if metadata.is_err() {
                    tracing::trace!("dirlistjson: Stat symlink target fail: {:#?}", entry);
                    continue;
                }
                let metadata = metadata.unwrap();
                is_dir = metadata.isdir();
                is_file = metadata.isfile();
            } else {
                is_dir = entry.isdir();
                is_file = entry.isfile();
            }

            // Skip weird files
            if !is_dir && !is_symlink && !is_file {
                continue;
            }

            // Probe the properties
            // If symlink, those of the symlink itself WITHOUT following.
            let name = name.unwrap();
            let name = name.to_string_lossy().to_string();
            let lastmod = entry.lastmod();
            if lastmod.is_none() {
                continue;
            }
            let lastmod = lastmod.unwrap();
            // Use RFC3339 format for last modified time using
            // Z+ for timezone ("true")
            let lastmod_httpstr = lastmod.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

            // Calculate the navigation (url) and thumbnail URL (thumb_url)
            let path = entry.path().context("dirlistjson: Did not expect path to be None when basename is Some").unwrap();
            // Make path relative to the root
            let path = path.strip_prefix(parent_path.as_ref());
            if let Err(ref e) = path {
                // Since we haven't followed symlinks, this should not happen.
                // If this does happen, it's a bug.
                // Still, treat as though an I/O error -> NotFound
                return Err(UnifiedError::NotFound(
                    anyhow::anyhow!("dirlistjson: Failed to strip prefix: {}", e)
                ));
            }
            let path = path.unwrap();

            // If regular file, put to files
            if is_file {
                // The URL is "/{}" --- this allows downloading or viewing
                let url = Path::new("/").join(path);

                // Guess whether we can generate a thumbnail
                // by looking at its extension
                let can_thumb;
                let ext = path.extension();
                if let Some(ext) = ext {
                    if ext.len() > 4 {
                        // Longer than jpeg, webp.
                        can_thumb = false;
                    } else {
                        let ext = ext.as_bytes();
                        can_thumb =
                            ext.eq_ignore_ascii_case(b"jpg") ||
                            ext.eq_ignore_ascii_case(b"jpeg") ||
                            ext.eq_ignore_ascii_case(b"png") ||
                            ext.eq_ignore_ascii_case(b"gif") ||
                            ext.eq_ignore_ascii_case(b"webp");
                    }
                } else {
                    can_thumb = false;
                }
                // If we can make custom thumbnail, "/thumb/{}".
                // Otherwise, "/thumb".
                let thumb_url = if can_thumb {
                    Path::new("/thumb").join(path)
                } else {
                    Path::new("/thumb").to_path_buf()
                };

                files.push(json!({
                    "name": name,
                    "last_modified": lastmod_httpstr,
                    "url": url,
                    "thumb_url": thumb_url,
                }));
                continue;
            }

            // If directory, put to directories
            if is_dir {
                // The URL is "/{}" --- this allows browsing
                let url = Path::new("/").join(path);

                // Directories have a fixed thumbnail of "/thumbdir"
                let thumb_url = Path::new("/thumbdir");

                directories.push(json!({
                    "name": name,
                    "last_modified": lastmod_httpstr,
                    "url": url,
                    "thumb_url": thumb_url,
                }));
                continue;
            }
        }

        // Shuffle the arrays
        let mut rng = rand::thread_rng();
        files.shuffle(&mut rng);
        directories.shuffle(&mut rng);

        Ok(json!({
            "files": files,
            "directories": directories,
            "truncated": truncated,
        }))
    })
    .await
    .context("dirlistjson: join error")?
}
