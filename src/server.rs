use crate::data::{
    list_files_with_options, resolve_syncable_file_path_with_options, FileAccessOptions,
};
use crate::network;
use anyhow::Result;
use axum::extract::RawQuery;
use axum::http::{header, Method, StatusCode};
use axum::{response::IntoResponse, routing::get, Json, Router};
use percent_encoding::percent_decode_str;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::oneshot::{Receiver, Sender};
use tower_http::cors::{Any, CorsLayer};
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::debug;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub root_dir: PathBuf,
    pub selected_roots: Vec<String>,
    pub port: u16,
    pub allow_top_level_directory_symlinks: bool,
}

impl ServerConfig {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            selected_roots: Vec::new(),
            port: network::listen_port(),
            allow_top_level_directory_symlinks: false,
        }
    }

    pub fn with_selected_roots(mut self, selected_roots: Vec<String>) -> Self {
        self.selected_roots = selected_roots;
        self
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_allow_top_level_directory_symlinks(mut self, allow: bool) -> Self {
        self.allow_top_level_directory_symlinks = allow;
        self
    }

    fn file_access_options(&self) -> FileAccessOptions {
        FileAccessOptions {
            allow_top_level_directory_symlinks: self.allow_top_level_directory_symlinks,
        }
    }
}

async fn shutdown_signal(signal: Receiver<bool>) {
    tokio::select! {
        _ = signal => {},
    }
}

pub async fn run_server(
    config: ServerConfig,
    shutdown: Receiver<bool>,
    on_ready: Option<Sender<Result<SocketAddr, String>>>,
) -> Result<()> {
    if config.root_dir.as_os_str().is_empty() {
        return Ok(());
    }

    let cors = CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_origin(Any);

    let app = build_router(config.clone()).layer(cors);

    let addr = match network::preferred_bind_addr_for_port(config.port) {
        Ok(addr) => addr,
        Err(e) => {
            let message =
                "Private LAN sharing is unavailable. Connect to Wi-Fi or Ethernet on your local network.".to_string();
            eprintln!("Failed to determine private LAN bind address: {e}");
            if let Some(tx) = on_ready {
                let _ = tx.send(Err(message));
            }
            return Ok(());
        }
    };

    let listener = loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => break listener,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    eprintln!(
                        "Port {} on {} in use, retrying in 100ms...",
                        config.port,
                        addr.ip()
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                eprintln!("Failed to bind to {addr}: {e}");
                if let Some(tx) = on_ready {
                    let _ = tx.send(Err(format!(
                        "Could not start local sharing on {}:{}",
                        addr.ip(),
                        config.port
                    )));
                }
                return Ok(());
            }
        }
    };

    let bound_addr = listener.local_addr()?;
    if let Some(tx) = on_ready {
        let _ = tx.send(Ok(bound_addr));
    }

    axum::serve(listener, app.layer(TraceLayer::new_for_http()))
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await?;

    Ok(())
}

async fn list_files_handler(
    dir_path: PathBuf,
    selected_paths: Vec<String>,
    file_access_options: FileAccessOptions,
) -> impl IntoResponse {
    let all_files =
        list_files_with_options(dir_path, file_access_options).expect("can't list files");
    let files = all_files
        .into_iter()
        .filter(|f| {
            selected_paths.is_empty() || selected_paths.iter().any(|p| f.path.starts_with(p))
        })
        .collect::<Vec<_>>();
    Json(files)
}

fn extract_path_param(raw_query: Option<&str>) -> Option<String> {
    // Parse `path=...` from the raw query string using pure percent-decoding.
    // We deliberately avoid form-decoding (which maps `+` → space) because
    // filenames can legitimately contain `+` characters.
    raw_query?.split('&').find_map(|kv| {
        let value = kv.strip_prefix("path=")?;
        Some(percent_decode_str(value).decode_utf8_lossy().into_owned())
    })
}

async fn download_file_query_handler(
    dir_path: PathBuf,
    file_access_options: FileAccessOptions,
    RawQuery(raw_query): RawQuery,
) -> impl IntoResponse {
    let path = match extract_path_param(raw_query.as_deref()) {
        Some(p) => p,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    debug!("download request for path: {:?}", path);
    let resolved_path =
        match resolve_syncable_file_path_with_options(&dir_path, &path, file_access_options) {
            Ok(path) => path,
            Err(e) => {
                debug!("download rejected for {:?}: {}", path, e);
                return StatusCode::NOT_FOUND.into_response();
            }
        };

    match tokio::fs::read(&resolved_path).await {
        Ok(data) => {
            debug!("serving {} bytes from {:?}", data.len(), resolved_path);
            ([(header::CONTENT_TYPE, "application/octet-stream")], data).into_response()
        }
        Err(e) => {
            debug!("failed to read file {:?}: {}", resolved_path, e);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

pub fn build_router(config: ServerConfig) -> Router {
    // Deprecated: retained for older clients that still request /file/<path>.
    // New clients should download through /file-by-path?path=..., and this route
    // will be removed in a future update.
    let deprecated_file_service = ServeDir::new(config.root_dir.clone());

    let file_access_options = config.file_access_options();
    let files_dir = config.root_dir.clone();
    let files_selected_paths = config.selected_roots.clone();
    let query_dir = config.root_dir;

    Router::new()
        .nest_service("/file", deprecated_file_service)
        .route(
            "/file-by-path",
            get(move |query| {
                download_file_query_handler(query_dir.clone(), file_access_options, query)
            }),
        )
        .route(
            "/files",
            get(move || {
                list_files_handler(
                    files_dir.clone(),
                    files_selected_paths.clone(),
                    file_access_options,
                )
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::{build_router, extract_path_param, run_server, ServerConfig};
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::oneshot;
    use tower::util::ServiceExt;

    fn temp_test_dir(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("minimoon-sync-server-{prefix}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn extract_path_param_treats_plus_as_literal() {
        assert_eq!(
            extract_path_param(Some("path=A+B.mp3")),
            Some("A+B.mp3".to_string())
        );
    }

    #[test]
    fn extract_path_param_percent_decodes_spaces() {
        assert_eq!(
            extract_path_param(Some("path=A%20B.mp3")),
            Some("A B.mp3".to_string())
        );
    }

    #[test]
    fn extract_path_param_handles_missing_path_key() {
        assert_eq!(extract_path_param(Some("other=foo")), None);
        assert_eq!(extract_path_param(None), None);
    }

    #[test]
    fn extract_path_param_finds_path_key_not_first() {
        assert_eq!(
            extract_path_param(Some("other=foo&path=test.mp3")),
            Some("test.mp3".to_string())
        );
    }

    #[test]
    fn extract_path_param_percent_decodes_equals_in_filename() {
        // "a=b.mp3" encoded as "a%3Db.mp3" — we decode *after*
        // splitting on '=', so the real '=' in the filename is preserved.
        assert_eq!(
            extract_path_param(Some("path=a%3Db.mp3")),
            Some("a=b.mp3".to_string())
        );
    }

    #[tokio::test]
    async fn file_by_path_returns_file_contents_for_encoded_query_paths() {
        let root = temp_test_dir("download");
        let nested_dir = root.join("Albums");
        fs::create_dir_all(&nested_dir).unwrap();
        let file_path = nested_dir.join("track #1?.opus");
        fs::write(&file_path, "hello").unwrap();

        let app = build_router(ServerConfig::new(&root));
        let request: Request<Body> = Request::builder()
            .uri("/file-by-path?path=Albums%2Ftrack%20%231%3F.opus")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"hello");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn file_by_path_treats_plus_as_literal_not_space() {
        let root = temp_test_dir("plus");
        fs::write(root.join("A+B.mp3"), "hello").unwrap();

        let app = build_router(ServerConfig::new(&root));
        // `+` in the query value must resolve to the file named "A+B.mp3",
        // not "A B.mp3" (which would be the form-decode interpretation).
        let request: Request<Body> = Request::builder()
            .uri("/file-by-path?path=A+B.mp3")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"hello");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn file_by_path_rejects_parent_directory_traversal() {
        let root = temp_test_dir("traversal");
        let app = build_router(ServerConfig::new(&root));
        let request: Request<Body> = Request::builder()
            .uri("/file-by-path?path=..%2Fsecret.opus")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), 404);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn file_by_path_rejects_empty_path_value() {
        let root = temp_test_dir("empty-path");
        fs::write(root.join("track.mp3"), "hello").unwrap();

        let app = build_router(ServerConfig::new(&root));
        let request: Request<Body> = Request::builder()
            .uri("/file-by-path?path=")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), 404);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_by_path_rejects_top_level_directory_symlinks_by_default() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("symlink-default");
        let outside = temp_test_dir("symlink-default-outside");
        fs::write(outside.join("track.mp3"), "hello").unwrap();
        symlink(&outside, root.join("Artist")).unwrap();

        let app = build_router(ServerConfig::new(&root));
        let request: Request<Body> = Request::builder()
            .uri("/file-by-path?path=Artist%2Ftrack.mp3")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), 404);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_by_path_allows_top_level_directory_symlinks_when_enabled() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("symlink-enabled");
        let outside = temp_test_dir("symlink-enabled-outside");
        fs::write(outside.join("track.mp3"), "hello").unwrap();
        symlink(&outside, root.join("Artist")).unwrap();

        let app =
            build_router(ServerConfig::new(&root).with_allow_top_level_directory_symlinks(true));
        let request: Request<Body> = Request::builder()
            .uri("/file-by-path?path=Artist%2Ftrack.mp3")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"hello");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[tokio::test]
    async fn run_server_serves_files_on_ready_bound_address() {
        let root = temp_test_dir("integration");
        fs::write(root.join("track.mp3"), "hello").unwrap();

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let task = tokio::spawn(run_server(
            ServerConfig::new(&root).with_port(0),
            shutdown_rx,
            Some(ready_tx),
        ));

        let addr = ready_rx.await.unwrap().unwrap();
        let response: Vec<crate::FileInfo> = reqwest::get(format!("http://{addr}/files"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response.len(), 1);
        assert_eq!(response[0].path, "track.mp3");

        let body = reqwest::get(format!("http://{addr}/file-by-path?path=track.mp3"))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(body.as_ref(), b"hello");

        let _ = shutdown_tx.send(true);
        task.await.unwrap().unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
