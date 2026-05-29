use crate::data::{list_files, resolve_syncable_file_path};
use crate::network;
use anyhow::Result;
use axum::extract::Query;
use axum::http::{header, Method, StatusCode};
use axum::{response::IntoResponse, routing::get, Json, Router};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::oneshot::{Receiver, Sender};
use tower_http::cors::{Any, CorsLayer};
use tower_http::{services::ServeDir, trace::TraceLayer};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub root_dir: PathBuf,
    pub selected_roots: Vec<String>,
    pub port: u16,
}

impl ServerConfig {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            selected_roots: Vec::new(),
            port: network::listen_port(),
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
}

#[derive(Deserialize)]
struct FilePathQuery {
    path: String,
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

async fn list_files_handler(dir_path: PathBuf, selected_paths: Vec<String>) -> impl IntoResponse {
    let all_files = list_files(dir_path).expect("can't list files");
    let files = all_files
        .into_iter()
        .filter(|f| {
            selected_paths.is_empty() || selected_paths.iter().any(|p| f.path.starts_with(p))
        })
        .collect::<Vec<_>>();
    Json(files)
}

async fn download_file_query_handler(
    dir_path: PathBuf,
    Query(query): Query<FilePathQuery>,
) -> impl IntoResponse {
    let resolved_path = match resolve_syncable_file_path(&dir_path, &query.path) {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    match tokio::fs::read(&resolved_path).await {
        Ok(data) => ([(header::CONTENT_TYPE, "application/octet-stream")], data).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

pub fn build_router(config: ServerConfig) -> Router {
    let serve_dir_from_dist = ServeDir::new(config.root_dir.clone());

    let files_dir = config.root_dir.clone();
    let files_selected_paths = config.selected_roots.clone();
    let query_dir = config.root_dir;

    Router::new()
        .nest_service("/file", serve_dir_from_dist)
        .route(
            "/file-by-path",
            get(move |query| download_file_query_handler(query_dir.clone(), query)),
        )
        .route(
            "/files",
            get(move || list_files_handler(files_dir.clone(), files_selected_paths.clone())),
        )
}

#[cfg(test)]
mod tests {
    use super::{build_router, run_server, ServerConfig};
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
