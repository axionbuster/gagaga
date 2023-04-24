//! File Lister --- list files in a directory (don't download)

use std::path::PathBuf;

use tokio::join;
use tower_http::trace::TraceLayer;

mod api;
mod fs;
mod prim;
mod thumb;

#[tokio::main]
async fn main() {
    // Init logging
    tracing_subscriber::fmt::init();
    let tracer = TraceLayer::new_for_http();

    let chroot = PathBuf::from("/");

    // Bind list at 2999
    let list = api::build_list_api(chroot.clone()).layer(tracer.clone());
    let list = axum::Server::bind(&"127.0.0.1:2999".parse().unwrap())
        .serve(list.into_make_service());
    let list = async move { list.await.unwrap() };

    // Bind thumb at 2998
    let thumb = api::build_thumb_api(chroot.clone()).layer(tracer.clone());
    let thumb = axum::Server::bind(&"127.0.0.1:2998".parse().unwrap())
        .serve(thumb.into_make_service());
    let thumb = async move { thumb.await.unwrap() };

    // Download server at 2997
    let download = api::build_download_api(chroot).layer(tracer);
    let download = axum::Server::bind(&"127.0.0.1:2997".parse().unwrap())
        .serve(download.into_make_service());
    let download = async move { download.await.unwrap() };

    // Go
    join!(list, thumb, download);
}
