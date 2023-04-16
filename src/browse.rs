//! Single page app + static media

use axum::response::IntoResponse;
use tokio::sync::OnceCell;

use crate::primitive::{anyhow::Context, tracing::instrument, *};

/// Global copy of scripts.js, rendered once at startup.
pub static SCRIPTS_JS: OnceCell<String> = OnceCell::const_new();

/// Serve the index page.
#[instrument]
pub async fn serve_index() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("index.html"))
}

/// Serve styles.css.
#[instrument]
pub async fn serve_styles() -> axum::response::Response {
    axum::response::Response::builder()
        .header("Content-Type", "text/css")
        .body(axum::body::Body::from(include_str!("styles.css")))
        .context("serve_styles: make response")
        .unwrap()
        .into_response()
}

/// Serve scripts.js
#[instrument]
pub async fn serve_scripts() -> axum::response::Response {
    axum::response::Response::builder()
        .header("Content-Type", "text/javascript")
        .body(axum::body::Body::from(
            SCRIPTS_JS
                .get()
                .expect("scripts.js should have been rendered at startup")
                .as_str(),
        ))
        .context("serve_scripts: make response")
        .unwrap()
        .into_response()
}
