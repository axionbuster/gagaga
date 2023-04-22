//! File Lister --- list files in a directory (don't download)

mod api;
mod prim;
mod thumb;
mod vfs;

#[tokio::main]
async fn main() {
    // Init logging
    tracing_subscriber::fmt::init();

    // Hi
    tracing::info!("File Lister");
}
