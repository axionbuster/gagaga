//! Serve files from a directory

use axum::{
    // debug_handler, // (useful for debugging obscure type errors)
    middleware::{map_request, map_response},
    routing::get,
    Router,
};
use tower_http::trace::TraceLayer;

mod app; // Routing and State
mod cachethumb; // Cache thumbnails
mod domain; // Domain types and processes
mod primitive; // Primitives + essential dependencies
mod vfs; // Virtual File System
mod vfsa; // Virtual File System (strategy "A")
mod vfspanic; // Virtual File System (strategy "panic")

use crate::{
    app::*,
    primitive::{anyhow::Context, tracing::instrument},
    vfs::{PathBuf, VfsV1},
    vfsa::VfsImplA,
};

#[tokio::main]
#[instrument]
async fn main() {
    // Set up logging
    tracing_subscriber::fmt::init();

    // Before building app, ROOT must be set. It is the root directory
    // serving data.
    // First, check the arguments. We make a few assumptions.
    //  1. The first argument is the path to the executable.
    //  2. The second argument is the path to the root directory. <--- what we want.
    //  3. No glob patterns are used from the perspective of the program.
    //  (Unix/Linux shells typically expand them before passing them onto us.
    //   Windows shells typically don't expand them at all.)
    let args: Vec<String> = std::env::args().collect();
    let root = if args.len() < 2 {
        // Let the user know that the program expects a path to the root directory.
        // Still, we will use the current directory as the root directory.
        tracing::info!(
            "No root directory specified. Using current directory. \
Usage: ./(program) (root directory)"
        );
        std::env::current_dir().unwrap()
    } else {
        tracing::info!("Root directory specified: {arg:?}", arg = &args[1]);
        // TODO: Let the user know if the path uses glob patterns.
        let temp = PathBuf::from(&args[1]);
        VfsImplA
            .canonicalize(&temp)
            .await
            .context("The root directory as specified failed to canonicalize")
            .unwrap()
    };
    tracing::info!("Serving at {root:?}");

    let root = domain::RealPath::from_trusted_pathbuf(root);
    ROOT.set(root).unwrap();

    // Also, primitively cache the thumbnails.
    let cache_mpsc = cachethumb::spawn_cache_process();
    CACHEMPSC.set(cache_mpsc).unwrap();

    // Build app
    let app = Router::new()
        .merge(
            // Static assets
            Router::new()
                .route("/user", get(serve_index))
                .route("/user/", get(serve_index))
                .route("/thumb", get(serve_svg_file_icon))
                .route("/thumb/", get(serve_svg_file_icon))
                .route("/thumbdir", get(serve_svg_folder_icon))
                .route("/thumbdir/", get(serve_svg_folder_icon))
                .route("/thumbimg", get(serve_loading_png))
                .route("/thumbimg/", get(serve_loading_png))
                .route("/styles.css", get(serve_styles))
                .route("/scripts.js", get(serve_scripts))
                // ... with HTTP caching
                .layer(map_response(add_static_cache_control)),
        )
        .merge(
            Router::new()
                .route("/root", get(serve_root))
                .route("/root/", get(serve_root))
                .route("/root/*userpath", get(serve_root))
                // Special route for dynamic thumbnails
                .route("/thumb/*userpath", get(serve_thumb::<_, 200, 200>))
                // Browse
                .route("/user/*userpath", get(serve_index)) // ignore userpath
                .layer(map_request(resolve_path)),
        )
        .fallback(get(serve_index))
        .layer(TraceLayer::new_for_http());

    // Start server, listening on port 3000
    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
