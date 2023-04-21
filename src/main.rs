//! File Lister --- list files in a directory (don't download)

#[tokio::main]
async fn main() {
    // Init logging
    tracing_subscriber::fmt::init();

    // Hi
    tracing::info!("File Lister");
}
