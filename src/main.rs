//! File Lister --- list files in a directory (don't download)

use std::path::PathBuf;

use tokio::join;
use tower_http::trace::TraceLayer;

mod api;
mod fs;
mod prim;
mod thcache;
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
    let thumb = api::build_thumb_api(chroot).layer(tracer);
    let thumb = axum::Server::bind(&"127.0.0.1:2998".parse().unwrap())
        .serve(thumb.into_make_service());
    let thumb = async move { thumb.await.unwrap() };

    // Go
    join!(list, thumb);
}
