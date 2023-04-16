//! Serve files from a directory

use axum::{
    http::{HeaderValue, Method},
    // debug_handler, // (useful for debugging obscure type errors)
    middleware::{map_request, map_response},
    routing::get,
    Router,
};
use clap::Parser;
use tokio::{join, task::JoinHandle};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

mod browse; // Single page app + static media
mod cachethumb; // Cache thumbnails
mod domain; // Domain types and processes
mod list; // List files in a directory
mod primitive; // Primitives + essential dependencies
mod thumb; // Thumbnail generation
mod vfs; // Virtual File System
mod vfsa; // Virtual File System (strategy "A")
mod vfspanic; // Virtual File System (strategy "panic")

use crate::{
    browse::SCRIPTS_JS,
    domain::RealPath,
    list::ROOT,
    primitive::{anyhow::Context, tracing::instrument, Result},
    thumb::CACHEMPSC,
    vfs::{PathBuf, VfsV1},
    vfsa::VfsImplA,
};

/// Command-line arguments
#[derive(Parser, Debug)]
struct Args {
    // Three services: should any of these be disabled?
    /// Disable the browse service
    #[clap(long)]
    no_browse: bool,

    /// Disable the list service
    #[clap(long)]
    no_list: bool,

    /// Disable the thumbnail service
    #[clap(long)]
    no_thumb: bool,

    /// Browse: Where to bind to?
    #[clap(long, default_value = "127.0.0.1:3000")]
    browse_bind: String,

    /// Browse: What is the origin for the list service?
    #[clap(long, default_value = "http://localhost:2999")]
    browse_list_origin: String,

    /// Browse: What is the origin for the thumbnail service?
    /// (default: http://localhost:3001)
    #[clap(long, default_value = "http://localhost:3001")]
    browse_thumb_origin: String,

    /// List: Where to bind to?
    #[clap(long, default_value = "127.0.0.1:2999")]
    list_bind: String,

    /// List: What is the root directory (local by default)?
    #[clap(long)]
    list_root: Option<PathBuf>,

    /// List: What is the CORS origin for the browse service?
    #[clap(long, default_value = "http://localhost:3000")]
    list_browse_origin: String,

    /// Thumb: Where to bind to?
    #[clap(long, default_value = "127.0.0.1:3001")]
    thumb_bind: String,
}

#[instrument]
fn up_list(
    list_root: Option<PathBuf>,
    browse_origin: String,
    bind: String,
) -> Result<JoinHandle<()>> {
    // Use the given service directory (canonicalize it, too).
    // If not given, use local directory.
    // If fails, stop.
    let root = if let Some(ref root_arg) = list_root {
        VfsImplA
            .canonicalizesync(root_arg)
            .context("Failed to canonicalize root directory")
    } else {
        std::env::current_dir().context("Failed to get current directory")
    }?;
    tracing::info!("Root directory: {:?}", root);

    // Update global state
    ROOT.set(RealPath::from_trusted_pathbuf(root)).unwrap();

    // Build CORS layer
    let cors = CorsLayer::new()
        .allow_origin(browse_origin.parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::HEAD]);

    // Build list service (JSON API)
    let list_app = Router::new()
        .route("/", get(list::serve_root))
        .route("/*userpath", get(list::serve_root))
        // cors
        .layer(cors)
        // automatically resolve & validate paths
        .layer(map_request(list::resolve_path))
        // ... with tracing
        .layer(TraceLayer::new_for_http());

    // Spawn list service (JSON API)
    Ok(tokio::spawn(async move {
        tracing::info!("List service to listen on {}", bind);
        axum::Server::bind(&bind.parse().unwrap())
            .serve(list_app.into_make_service())
            .await
            .unwrap();
    }))
}

#[instrument]
fn up_thumb(bind: String) -> JoinHandle<()> {
    // Spawn thumbnail "process" (thread), and retrieve "pipe" (mpsc channel).
    let cache_mpsc = cachethumb::spawn_cache_process();
    CACHEMPSC.set(cache_mpsc).unwrap();

    // Build thumbnail service (JSON API)
    let thumb_app = Router::new()
        .route("/thumb", get(thumb::serve_svg_file_icon))
        .route("/thumb/", get(thumb::serve_svg_file_icon))
        .route("/thumbdir", get(thumb::serve_svg_folder_icon))
        .route("/thumbdir/", get(thumb::serve_svg_folder_icon))
        .route("/thumbimg", get(thumb::serve_loading_png))
        .route("/thumbimg/", get(thumb::serve_loading_png))
        .route("/thumb/*userpath", get(thumb::serve_thumb::<_, 64, 64>))
        // automatically resolve & validate paths
        .layer(map_request(list::resolve_path))
        // ... with tracing
        .layer(TraceLayer::new_for_http());

    tokio::spawn(async move {
        tracing::info!("Thumbnail service to listen on {}", bind);
        axum::Server::bind(&bind.parse().unwrap())
            .serve(thumb_app.into_make_service())
            .await
            .unwrap();
    })
}

#[instrument]
fn up_browse(
    list_origin: String,
    thumb_origin: String,
    bind: String,
) -> Result<JoinHandle<()>> {
    use sailfish::TemplateOnce;

    // Templating for JavaScript
    #[derive(TemplateOnce)]
    #[template(path = "scripts.js")]
    struct ScriptsJs {
        list_origin: String,
        thumb_origin: String,
    }
    let scripts_js = ScriptsJs {
        list_origin,
        thumb_origin,
    }
    .render_once()
    .context("Failed to render scripts.js using a template")?;

    tracing::debug!("Rendered scripts.js: {} bytes", scripts_js.len());

    // Register template
    SCRIPTS_JS.set(scripts_js).unwrap();

    // Build browse service (single page app)
    let browse_app = Router::new()
        .route("/styles.css", get(browse::serve_styles))
        .route("/scripts.js", get(browse::serve_scripts))
        // ... fallback to index.html
        .fallback(get(browse::serve_index))
        // ... with HTTP caching
        .layer(map_response(thumb::add_static_cache_control))
        // ... with tracing
        .layer(TraceLayer::new_for_http());

    // Spawn a separate thread for each service
    Ok(tokio::spawn(async move {
        tracing::info!("Browse service to listen on {}", bind);
        axum::Server::bind(&bind.parse().unwrap())
            .serve(browse_app.into_make_service())
            .await
            .unwrap();
    }))
}

#[tokio::main]
#[instrument]
async fn main() -> Result<()> {
    // Set up logging
    tracing_subscriber::fmt::init();

    // Parse command-line arguments
    let args = Args::parse();

    tracing::debug!("Received arguments: {:?}", args);

    // List service
    let list = if !args.no_list {
        tracing::info!("List service enabled (use --no-list to disable)");
        Some(up_list(
            args.list_root,
            args.list_browse_origin,
            args.list_bind,
        )?)
    } else {
        tracing::info!("List service disabled");
        None
    };

    let thumb = if !args.no_thumb {
        tracing::info!("Thumbnail service enabled (use --no-thumb to disable)");
        Some(up_thumb(args.thumb_bind))
    } else {
        tracing::info!("Thumbnail service disabled");
        None
    };

    let browse = if !args.no_browse {
        tracing::info!("Browse service enabled (use --no-browse to disable)");
        Some(up_browse(
            args.browse_list_origin,
            args.browse_thumb_origin,
            args.browse_bind,
        )?)
    } else {
        tracing::info!("Browse service disabled");
        None
    };

    // If none of them are enabled, stop.
    if list.is_none() && thumb.is_none() && browse.is_none() {
        tracing::info!("No services enabled, exiting");
        return Ok(());
    }

    // Wait for all services to exit
    // by awaiting on the join handles
    let b = async move {
        if let Some(browse) = browse {
            browse.await.unwrap();
        }
    };
    let l = async move {
        if let Some(list) = list {
            list.await.unwrap();
        }
    };
    let t = async move {
        if let Some(thumb) = thumb {
            thumb.await.unwrap();
        }
    };
    tracing::info!("SERVICE IS GOING UP! YAY!");
    join!(b, l, t);

    // Shutting down
    tracing::info!("Shutting down");

    Ok(())
}
