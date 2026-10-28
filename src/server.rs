use crate::configuration::Settings;
use crate::file_manager::{normalize_hosts, parse_csv_as_urls, validate_url};
use crate::live_crawler::{UrlSender, spawn_live_crawler};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Local;
use futures::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::{mpsc::error::TrySendError, oneshot};
use tower_http::decompression::RequestDecompressionLayer;
use tracing::{error, info};

pub struct AppState {
    pub input_dir: PathBuf,
    pub allowed_hosts: Vec<String>,
    pub api_token: SecretString,
    pub max_upload_bytes: u64,
    pub url_tx: UrlSender,
}

pub async fn serve(config: Settings) {
    let server = config
        .server
        .clone()
        .expect("Add a [server] section to the config to use `serve` (see README)");

    if server.api_token.expose_secret().trim().is_empty() {
        panic!("server.api_token is empty in the config.");
    }

    //---------- Same rule as `run`: nothing could be crawled without allowed hosts
    let allowed_hosts = normalize_hosts(&config.application.allowed_hosts);
    if allowed_hosts.is_empty() {
        panic!("allowed_hosts is empty in the config, so no URL could be crawled.");
    }

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (url_tx, worker) = spawn_live_crawler(config.clone(), server.queue_capacity, async {
        let _ = shutdown_rx.await;
    });

    let state = Arc::new(AppState {
        input_dir: PathBuf::from(&config.application.input_dir),
        allowed_hosts,
        api_token: server.api_token.clone(),
        max_upload_bytes: server.max_upload_mb * 1024 * 1024,
        url_tx,
    });

    let listener = TcpListener::bind((server.host.as_str(), server.port))
        .await
        .expect("Failed to bind the server address");
    info!("Listening on {}", listener.local_addr().unwrap());

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("Server error");

    //---------- URLs still waiting in the /urls queue are dropped. Uploaded files are already on disk
    info!("Shutting down, stopping the live crawl");
    let _ = shutdown_tx.send(());
    if let Err(e) = worker.await {
        error!("Live crawler task panicked: {e}");
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/upload", post(upload))
        .route("/urls", post(queue_urls))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        //---------- Bodies sent with Content-Encoding: gzip are decompressed before the handlers read them,
        //---------- so the size limits below apply to the decompressed size (no gzip bombs)
        .layer(RequestDecompressionLayer::new())
        .layer(DefaultBodyLimit::max(state.max_upload_bytes as usize))
        .with_state(state)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to listen for SIGTERM")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let queued = state.url_tx.max_capacity() - state.url_tx.capacity();
    Json(json!({ "status": "ok", "queued_urls": queued }))
}

//---------- Auth

async fn require_token(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);

    match provided {
        Some(token)
            if constant_time_eq(
                token.as_bytes(),
                state.api_token.expose_secret().trim().as_bytes(),
            ) =>
        {
            next.run(req).await
        }
        _ => error_response(StatusCode::UNAUTHORIZED, "Missing or invalid bearer token"),
    }
}

/// Compares every byte so the time taken doesn't reveal how much of the token matched.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

//---------- POST /upload

#[derive(Deserialize)]
struct UploadQuery {
    name: Option<String>,
}

#[derive(Serialize)]
struct UploadResponse {
    file: String,
    urls: usize,
}

enum UploadError {
    TooLarge,
    Body(axum::Error),
    Io(std::io::Error),
}

impl From<std::io::Error> for UploadError {
    fn from(e: std::io::Error) -> Self {
        UploadError::Io(e)
    }
}

/// Unique per process, so two uploads in the same second don't overwrite each other.
static UPLOAD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Saves the body (plain or `Content-Encoding: gzip`) as a CSV in `input_dir`, to be crawled by the next `run`.
async fn upload(
    State(state): State<Arc<AppState>>,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> Response {
    let file_name = format!(
        "{}_{}_{}.csv",
        Local::now().format("%Y%m%d%H%M%S"),
        UPLOAD_COUNTER.fetch_add(1, Ordering::Relaxed),
        sanitize_file_stem(query.name.as_deref().unwrap_or_default())
    );

    //---------- Write to a hidden file first. `run` skips hidden files, so it never sees a partial upload
    let tmp_path = state.input_dir.join(format!(".{}.part", file_name));
    let final_path = state.input_dir.join(&file_name);

    if let Err(e) = write_body(&tmp_path, body, state.max_upload_bytes).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return match e {
            UploadError::TooLarge => error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Upload is larger than {} MB once decompressed",
                    state.max_upload_bytes / 1024 / 1024
                ),
            ),
            UploadError::Body(e) => error_response(
                StatusCode::BAD_REQUEST,
                format!("Could not read the body (invalid gzip?): {e}"),
            ),
            UploadError::Io(e) => {
                error!("Failed to save upload {:?}: {}", tmp_path, e);
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save the file")
            }
        };
    }

    //---------- Reject files with nothing to crawl, so the sender finds out now instead of in the report
    let check_path = tmp_path.clone();
    let hosts = state.allowed_hosts.clone();
    let urls = tokio::task::spawn_blocking(move || {
        parse_csv_as_urls(check_path.to_str().unwrap_or_default(), &hosts).len()
    })
    .await
    .unwrap_or(0);

    if urls == 0 {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "No crawlable URLs in the file (one http(s) URL per line, host in allowed_hosts)",
        );
    }

    if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        error!("Failed to move upload to {:?}: {}", final_path, e);
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save the file");
    }

    info!("Saved upload {} with {} URLs", file_name, urls);
    (
        StatusCode::CREATED,
        Json(UploadResponse {
            file: file_name,
            urls,
        }),
    )
        .into_response()
}

async fn write_body(path: &Path, body: Body, max_bytes: u64) -> Result<(), UploadError> {
    let mut file = tokio::fs::File::create(path).await?;
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;

    //---------- Stream to disk, counting decompressed bytes, instead of holding the file in memory
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(UploadError::Body)?;
        written += chunk.len() as u64;
        if written > max_bytes {
            return Err(UploadError::TooLarge);
        }
        file.write_all(&chunk).await?;
    }

    file.flush().await?;
    file.sync_all().await?;
    Ok(())
}

/// Keeps only `[A-Za-z0-9_-]` from the client's name, so it can't escape `input_dir`.
fn sanitize_file_stem(name: &str) -> String {
    //---------- Drop extensions the client may send, e.g. urls.csv.gz -> urls
    let mut stem = name.trim();
    while let Some(ext) = [".gz", ".csv", ".txt"]
        .iter()
        .find(|ext| stem.to_ascii_lowercase().ends_with(*ext))
    {
        stem = &stem[..stem.len() - ext.len()];
    }

    let cleaned: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let cleaned = cleaned.trim_matches('_');

    if cleaned.is_empty() {
        String::from("upload")
    } else {
        cleaned.to_string()
    }
}

//---------- POST /urls

#[derive(Deserialize)]
struct UrlsRequest {
    urls: Vec<String>,
}

#[derive(Serialize)]
struct SkippedUrl {
    url: String,
    reason: String,
}

#[derive(Serialize)]
struct UrlsResponse {
    queued: usize,
    skipped: Vec<SkippedUrl>,
}

/// Validates the URLs and queues them for the live crawl. Answers right away, before they're crawled.
async fn queue_urls(State(state): State<Arc<AppState>>, Json(req): Json<UrlsRequest>) -> Response {
    let mut seen = HashSet::new();
    let mut valid = Vec::new();
    let mut skipped = Vec::new();

    for url in req.urls {
        let url = url.trim().to_string();
        if !seen.insert(url.clone()) {
            continue;
        }
        match validate_url(&url, &state.allowed_hosts) {
            Ok(()) => valid.push(url),
            Err(reason) => skipped.push(SkippedUrl { url, reason }),
        }
    }

    //---------- All or nothing when the queue is full, so the sender can simply retry the same request
    if state.url_tx.capacity() < valid.len() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "60")],
            Json(json!({ "error": "The crawl queue is full, retry later" })),
        )
            .into_response();
    }

    let mut queued = 0;
    for url in valid {
        match state.url_tx.try_send(url) {
            Ok(()) => queued += 1,
            //---------- Only possible if another request filled the queue in between
            Err(TrySendError::Full(url)) => skipped.push(SkippedUrl {
                url,
                reason: String::from("the crawl queue is full"),
            }),
            Err(TrySendError::Closed(_)) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "The crawler is shutting down",
                );
            }
        }
    }

    (StatusCode::ACCEPTED, Json(UrlsResponse { queued, skipped })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    const TOKEN: &str = "test-token";

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "magento_crawler_server_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn app(dir: &Path, max_upload_bytes: u64, queue: usize) -> (Router, mpsc::Receiver<String>) {
        let (url_tx, rx) = mpsc::channel(queue);
        let state = Arc::new(AppState {
            input_dir: dir.to_path_buf(),
            allowed_hosts: vec![String::from("example.com")],
            api_token: SecretString::from(TOKEN),
            max_upload_bytes,
            url_tx,
        });
        (router(state), rx)
    }

    fn request(uri: &str, token: Option<&str>, body: Vec<u8>) -> axum::http::request::Builder {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_LENGTH, body.len());
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        files.sort();
        files
    }

    #[test]
    fn sanitize_file_stem_strips_extensions_and_path_characters() {
        assert_eq!(sanitize_file_stem("urls.csv.gz"), "urls");
        assert_eq!(sanitize_file_stem("Daily URLs.TXT"), "Daily_URLs");
        assert_eq!(sanitize_file_stem("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_file_stem(""), "upload");
        assert_eq!(sanitize_file_stem("..."), "upload");
        assert_eq!(sanitize_file_stem(&"a".repeat(100)).len(), 64);
    }

    #[test]
    fn constant_time_eq_compares_whole_value() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let dir = test_dir("health");
        let (app, _rx) = app(&dir, 1024, 10);
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let res = app.oneshot(req).await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_routes_reject_missing_or_wrong_token() {
        let dir = test_dir("auth");
        let (app, _rx) = app(&dir, 1024, 10);

        for token in [None, Some("wrong")] {
            let body = b"https://example.com/\n".to_vec();
            let req = request("/upload", token, body.clone())
                .body(Body::from(body))
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        let files = files_in(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn upload_saves_gzipped_csv_atomically() {
        let dir = test_dir("upload_gzip");
        let (app, _rx) = app(&dir, 1024, 10);
        let csv = b"https://example.com/a\nhttps://evil.com/b\nhttps://example.com/c\n";
        let body = gzip(csv);
        let req = request("/upload?name=../daily.csv.gz", Some(TOKEN), body.clone())
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(body))
            .unwrap();

        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let files = files_in(&dir);
        let saved = std::fs::read(dir.join(&files[0])).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(json["urls"], 2);
        assert_eq!(files.len(), 1, "no .part file left behind: {files:?}");
        assert!(files[0].ends_with("_daily.csv"));
        assert_eq!(json["file"], files[0]);
        assert_eq!(saved, csv);
    }

    #[tokio::test]
    async fn upload_rejects_files_larger_than_limit_once_decompressed() {
        let dir = test_dir("upload_large");
        let (app, _rx) = app(&dir, 1024, 10);
        let csv = "https://example.com/a\n".repeat(1_000);
        let body = gzip(csv.as_bytes());
        assert!(body.len() < 1024, "compressed body fits the limit");
        let req = request("/upload", Some(TOKEN), body.clone())
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(body))
            .unwrap();

        let res = app.oneshot(req).await.unwrap();
        let files = files_in(&dir);
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn upload_rejects_files_without_crawlable_urls() {
        let dir = test_dir("upload_empty");
        let (app, _rx) = app(&dir, 1024, 10);
        let body = b"https://evil.com/\nnot a url\n".to_vec();
        let req = request("/upload", Some(TOKEN), body.clone())
            .body(Body::from(body))
            .unwrap();

        let res = app.oneshot(req).await.unwrap();
        let files = files_in(&dir);
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(files.is_empty());
    }

    fn urls_request(urls: &[&str]) -> Request<Body> {
        let body = serde_json::to_vec(&json!({ "urls": urls })).unwrap();
        request("/urls", Some(TOKEN), body.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn urls_queues_valid_unique_urls_and_reports_skipped() {
        let dir = test_dir("urls");
        let (app, mut rx) = app(&dir, 1024, 10);
        let req = urls_request(&[
            "https://example.com/a",
            " https://example.com/a ",
            "https://evil.com/b",
            "https://example.com/c",
        ]);

        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(json["queued"], 2);
        assert_eq!(json["skipped"][0]["url"], "https://evil.com/b");
        assert_eq!(rx.recv().await.unwrap(), "https://example.com/a");
        assert_eq!(rx.recv().await.unwrap(), "https://example.com/c");
    }

    #[tokio::test]
    async fn urls_returns_503_when_queue_cannot_take_the_whole_request() {
        let dir = test_dir("urls_full");
        let (app, mut rx) = app(&dir, 1024, 1);
        let req = urls_request(&["https://example.com/a", "https://example.com/b"]);

        let res = app.oneshot(req).await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(rx.try_recv().is_err(), "nothing was queued");
    }
}
