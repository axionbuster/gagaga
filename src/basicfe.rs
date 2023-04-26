//! Basic front-end
//!
//! (No thumbnails, list view)
//!
//! Also, an example of relying on the JSON responses and HTTP status
//! codes and not on the source code of the back-end.

use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{FromRequestParts, State},
    http::{request::Parts, Request},
    http::{HeaderValue, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::IntoResponse,
    response::Response,
    routing::get,
    Router,
};
use reqwest::Url;
use sailfish::TemplateOnce;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use time::{format_description::FormatItem, macros::format_description};

use crate::prim::*;

/// Basic error
#[derive(Debug, Error)]
#[error("Something went wrong")]
struct BasicError {
    /// Status Code
    code: StatusCode,
    /// Underlying error, if any
    #[source]
    err: Option<Error>,
}

/// Throw an error directly from a status code
impl<S: Into<StatusCode>> From<S> for BasicError {
    fn from(code: S) -> Self {
        Self {
            code: code.into(),
            err: None,
        }
    }
}

impl BasicError {
    /// Make an error from a status code and a comment
    ///
    /// The comment is not sent to the end user.
    fn from_status_comment<S: Into<StatusCode>>(code: S, msg: &str) -> Self {
        Self {
            code: code.into(),
            err: Some(anyhow!(msg.to_string())),
        }
    }
}

/// Extend Result so that anything can be converted into
/// Result<?, BasicError> with a given status code using
/// .with_status(c: S)
trait BasicResultExt<T, E> {
    /// Convert to a BasicError with a given status code
    fn with_status<S: Into<StatusCode>>(
        self,
        c: S,
    ) -> std::result::Result<T, BasicError>;
}

impl<T, E> BasicResultExt<T, E> for std::result::Result<T, E>
where
    E: Into<Error>,
{
    fn with_status<S: Into<StatusCode>>(
        self,
        c: S,
    ) -> std::result::Result<T, BasicError> {
        self.map_err(|e| BasicError {
            code: c.into(),
            err: Some(e.into()),
        })
    }
}

/// Allow a status code to be annotated and then converted into [`BasicError`].
trait StatusCodeExt {
    /// Annotate a status code with an error message
    fn annotate(self, e: &str) -> BasicError;

    /// Annotate a status code with a dynamically generated error from the
    /// given closure
    fn annotate_with<F: FnOnce() -> Error>(self, f: F) -> BasicError;
}

impl StatusCodeExt for StatusCode {
    fn annotate(self, e: &str) -> BasicError {
        BasicError {
            code: self,
            err: Some(anyhow!(e.to_string())),
        }
    }

    fn annotate_with<F: FnOnce() -> Error>(self, f: F) -> BasicError {
        BasicError {
            code: self,
            err: Some(f()),
        }
    }
}

/// Turn it into an Axum response
impl IntoResponse for BasicError {
    fn into_response(self) -> Response {
        (self.code, self.code.canonical_reason().unwrap_or_default())
            .into_response()
    }
}

type BasicResult<T> = std::result::Result<T, BasicError>;

/// Define a listed item (file, directory, etc.)
#[derive(Serialize, Debug)]
struct Item {
    /// Where to go as a link
    href: String,
    /// Name of the item
    name: String,
    /// Size of the item, in human readable amount, with units
    size_with_units: String,
    /// Last modified time of the item
    last_modified: String,
}

/// Define a page to be used as a template
#[derive(TemplateOnce)]
#[template(path = "basic.html")]
struct Page {
    root: String,
    time: String,
    directories: Vec<Item>,
    files: Vec<Item>,
}

/// Format a date and time in UNIX time for presentation to US English
/// speakers.
fn format_unix_timestamp(ts: i64) -> Result<String> {
    static FORMAT: &[FormatItem<'_>] =
        format_description!("[year]-[month]-[day] GMT");

    Ok(time::OffsetDateTime::from_unix_timestamp(ts)
        .context("convert UNIX timestamp to date")?
        .format(FORMAT)
        .unwrap())
}

/// Format the number of bytes into a human readable string for US
/// English speakers.
fn format_size_bytes(n: u64) -> String {
    let mut n = n as f64;
    let mut i = 0;
    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    while n >= 1024.0 {
        n /= 1024.0;
        i += 1;
    }
    format!("{:.2} {}", n, units[i])
}

/// Define each metadata file as in the JSON response.
#[derive(Debug)]
struct ApiFileMetadata {
    /// File name (base name)
    name: String,
    /// File size in bytes, if present.
    size: Option<u64>,
    /// Last modified time in UNIX time offset.
    ///
    /// The API gives out a "now" value, another UNIX timestamp,
    /// to find the correct last modified time---the value of adding both
    /// together.
    ///
    /// Follow the code to learn more.
    last_modified: Option<i64>,
}

/// Manually deserialize an [`ApiFileMetadata`] from an ordered
/// JSON array.
fn deser_api_file_metadata(array: &Value) -> Result<ApiFileMetadata> {
    let name = array
        .get(0)
        .ok_or_else(|| anyhow!("missing field [0], name"))?
        .as_str()
        .ok_or_else(|| anyhow!("field [0], name, is not a string"))?
        .to_owned();
    // ignore file type
    let size = array
        .get(2)
        .ok_or_else(|| anyhow!("missing field [2], size"))?
        .as_u64();
    let last_modified = array
        .get(3)
        .ok_or_else(|| anyhow!("missing field [3], last_modified"))?
        .as_i64();
    Ok(ApiFileMetadata {
        name,
        size,
        last_modified,
    })
}

/// Convert a [`ApiFileMetadata`] to an [`Item`] given the values of
/// the path (of the current directory) and the API-provided "now"
/// UNIX timestamp.
///
/// - `base`: Rooted (`/`) path. Such as `/Pictures/great neat pics`.
/// - `now`: The "now" field from the JSON response.
/// - `meta`: The metadata of the file.
fn show_api_file_metadata(
    base: &Path,
    now: i64,
    meta: ApiFileMetadata,
) -> Result<Item> {
    let href = base
        .join(&meta.name)
        .to_str()
        .ok_or_else(|| anyhow!("path not UTF-8"))?
        .to_owned();
    let name = meta.name;
    let size_with_units = meta
        .size
        .map(format_size_bytes)
        .unwrap_or_else(|| "".to_owned());
    let last_modified = meta
        .last_modified
        .map(|ts| ts + now)
        .map(format_unix_timestamp)
        .transpose()
        .expect("convert UNIX timestamp to date")
        .unwrap_or_else(|| "".to_owned());
    Ok(Item {
        href,
        name,
        size_with_units,
        last_modified,
    })
}

/// The download server's base URL
#[derive(Debug, Clone)]
struct DownloadBaseUrl(Arc<Url>);

/// Extract [`DownloadServerOrigin`] from the request.
#[async_trait]
impl FromRequestParts<()> for DownloadBaseUrl {
    type Rejection = BasicError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &(),
    ) -> BasicResult<Self> {
        let dso = parts.extensions.get::<DownloadBaseUrl>();
        if dso.is_none() {
            // Since DSO is our custom type, if we expect it but it doesn't
            // actually exist, it's our fault (logic error).
            return Err(StatusCode::INTERNAL_SERVER_ERROR.into());
        }
        Ok(dso.unwrap().clone())
    }
}

/// Inject a [`DownloadServerOrigin`] into the request from the
/// given argument.
async fn mw_inject_dso<B>(
    state_dso: State<DownloadBaseUrl>,
    mut req: Request<B>,
    next: Next<B>,
) -> impl IntoResponse {
    req.extensions_mut().insert(state_dso.0);
    next.run(req).await
}

/// List service base URL
#[derive(Debug, Clone)]
struct ListBaseUrl(Arc<Url>);

/// Extract [`ListBaseUrl`] from the request.
#[async_trait]
impl FromRequestParts<()> for ListBaseUrl {
    type Rejection = BasicError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &(),
    ) -> BasicResult<Self> {
        let lbo = parts.extensions.get::<ListBaseUrl>();
        if lbo.is_none() {
            // Since LBU is our custom type, if we expect it but it doesn't
            // actually exist, it's our fault (logic error).
            return Err(StatusCode::INTERNAL_SERVER_ERROR.into());
        }
        Ok(lbo.unwrap().clone())
    }
}

/// Inject a [`ListBaseUrl`] into the request from the
/// given argument.
async fn mw_inject_lbu<B>(
    state_lbu: State<ListBaseUrl>,
    mut req: Request<B>,
    next: Next<B>,
) -> impl IntoResponse {
    req.extensions_mut().insert(state_lbu.0);
    next.run(req).await
}

/// An HTTP Client connection pool.
///
/// See: [`reqwest::Client`].
#[derive(Debug, Clone)]
struct Client(reqwest::Client);

/// Extract [`reqwest::Client`] from the request.
#[async_trait]
impl FromRequestParts<()> for Client {
    type Rejection = BasicError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &(),
    ) -> BasicResult<Self> {
        let client = parts.extensions.get::<Client>();
        if client.is_none() {
            // Since Client is our custom type, if we expect it but it doesn't
            // actually exist, it's our fault (logic error).
            return Err(StatusCode::INTERNAL_SERVER_ERROR.into());
        }
        Ok(client.unwrap().clone())
    }
}

/// Inject a [`reqwest::Client`] to use while making HTTP requests to the
/// backend.
async fn mw_inject_http_client<B>(
    state_client: State<Client>,
    mut req: Request<B>,
    next: Next<B>,
) -> impl IntoResponse {
    req.extensions_mut().insert(state_client.0);
    next.run(req).await
}

/// Serve the HTTP (web) interface.
#[instrument(skip(client), err)]
async fn api(
    lbu: ListBaseUrl,
    dbu: DownloadBaseUrl,
    client: Client,
    path: Option<axum::extract::Path<PathBuf>>,
) -> BasicResult<Response> {
    // When the route is called without an argument declared at startup,
    // the path will be None. That is to mean the root directory.
    let path = path.map(|p| p.0).unwrap_or_else(|| PathBuf::from("/"));

    // Make the base path to be appended to each URL written to the HTML.
    // It is crucial that each one starts with a forward slash.
    let url_base_path = PathBuf::from("/").join(&path);

    // Try making a request to the LIST API. If it says, not found,
    // it could have been a file, so redirect the user temporarily
    // to the download server, then. Otherwise, if NOT 200,
    // then rely their status code or 500 if unknown. Otherwise,
    // say it's a directory and show the contents. Also, if the request
    // could not be made, then say 500 Internal Server Error.

    // Forge the URL.
    // Convert the path to a string.
    let path = path
        .to_str()
        .ok_or_err("path not UTF-8")
        .with_status(StatusCode::BAD_REQUEST)?;
    // Join the path with the list server base URL.
    let url = lbu
        .0
        .join(path)
        .context("join the path to list server base url")
        .with_status(StatusCode::BAD_REQUEST)?;
    // Make the request to the LIST service.
    let resp = client
        .0
        .get(url.clone())
        .send()
        .await
        .context("make the request to list service")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
    // Inspect the status code.
    let status = resp.status();
    // If 404, it could actually be a file not a directory. In that
    // case, make a redirect to the DOWNLOAD service.
    if status == StatusCode::NOT_FOUND {
        let url = dbu
            .0
            .join(path)
            .context("join the path to download server base url")
            .with_status(StatusCode::BAD_REQUEST)?;
        return Ok((
            StatusCode::TEMPORARY_REDIRECT,
            [("Location", HeaderValue::from_str(url.as_str()).unwrap())],
            "",
        )
            .into_response());
    }
    // If not 200, then it's an error.
    if status != StatusCode::OK {
        return Err(status.annotate("response not 200"));
    }

    // Fetch the JSON, and then interpret the result.

    // Fetch the JSON.
    let json: serde_json::Value =
        resp.json()
            .await
            .context("fetch the JSON")
            .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
    // Inspect the "version" and confirm that it exists, it's a string,
    // and that it begins with "04".
    let version = json
        .get("version")
        .ok_or_err("missing version")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?
        .as_str()
        .ok_or_err("version not string")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
    if !version.starts_with("04") {
        return Err(BasicError::from_status_comment(
            StatusCode::INTERNAL_SERVER_ERROR,
            "minor version not '4'",
        ));
    }
    // Fetch the "now", a UNIX timestamp.
    let now = json
        .get("now")
        .ok_or_err("missing now")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?
        .as_i64()
        .ok_or_err("now not integer")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
    // Display "now" as a date.
    let now_display = format_unix_timestamp(now)
        .context("format UNIX timestamp 'now' field")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
    // Grab the JSON array named "files," and then convert those into
    // Item's.
    let json_files = json
        .get("files")
        .ok_or_err("missing object key 'files'")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?
        .as_array()
        .ok_or_err("'files' not a JSON array")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut files = vec![];
    for json_file in json_files {
        let meta = deser_api_file_metadata(json_file)
            .context("deserialize API file metadata")
            .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
        let meta = show_api_file_metadata(&url_base_path, now, meta)
            .context("show API file metadata")
            .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
        files.push(meta);
    }
    // Do the same with "directories," except that the JSON field is
    // named "dirs," but our field is named "directories."
    let json_dirs = json
        .get("dirs")
        .ok_or_err("missing object key 'dirs'")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?
        .as_array()
        .ok_or_err("'dirs' not a JSON array")
        .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut directories = vec![];
    for json_dir in json_dirs {
        let meta = deser_api_file_metadata(json_dir)
            .context("deserialize API directory metadata")
            .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
        let meta = show_api_file_metadata(&url_base_path, now, meta)
            .context("show API directory metadata")
            .with_status(StatusCode::INTERNAL_SERVER_ERROR)?;
        directories.push(meta);
    }

    // Format the page

    let page = Page {
        root: url_base_path.to_string_lossy().to_string(),
        time: now_display,
        files,
        directories,
    };
    let page = page.render_once().expect(
        "expect the render to be successful due to \
static template validation",
    );
    let response = (
        [("Content-Type", HeaderValue::from_static("text/html"))],
        page,
    )
        .into_response();

    Ok(response)
}

/// Configure the API for the basic front-end
#[derive(Debug, Clone)]
pub struct BasicFrontend {
    /// The download server base URL
    pub download_base_url: String,
    /// The list server base URL
    pub list_base_url: String,
}

/// Serve
#[instrument]
pub fn build_api_basicfe(config: &BasicFrontend) -> Router<(), Body> {
    let dbu = Url::from_str(&config.download_base_url)
        .expect("expect the download base URL to be valid");
    let dbu = DownloadBaseUrl(Arc::new(dbu));

    let lbu = Url::from_str(&config.list_base_url)
        .expect("expect the list base URL to be valid");
    let lbu = ListBaseUrl(Arc::new(lbu));

    let client = Client(reqwest::Client::new());

    Router::new()
        .route("/*path", get(api))
        .route("/", get(api))
        .layer(from_fn_with_state(lbu, mw_inject_lbu))
        .layer(from_fn_with_state(dbu, mw_inject_dso))
        .layer(from_fn_with_state(client, mw_inject_http_client))
}
