use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use bytes::Bytes;
use miette::{miette, Context as _, IntoDiagnostic};
use serde::Serialize;
use tokio::net::TcpListener;

type AppState = Arc<RwLock<HashMap<String, Bytes>>>;

/// Run a minimal registry for local testing
async fn test_registry(listener: TcpListener, base_url: String) -> miette::Result<()> {
    let state = Arc::new(RwLock::new(HashMap::<String, Bytes>::new()));
    let app = Router::new()
        .route("/artifactory/api/search/artifact", get(search_artifacts))
        .route(
            "/*path",
            get(get_package).head(head_package).put(put_package),
        )
        .with_state(state)
        .layer(Extension(base_url));
    axum::serve(listener, app)
        .await
        .into_diagnostic()
        .wrap_err(miette!("failed to read the token from the user"))
}

/// Artifactory artifact search response format
#[derive(Serialize)]
struct ArtifactSearchResponse {
    results: Vec<ArtifactSearchResult>,
}

#[derive(Serialize)]
struct ArtifactSearchResult {
    uri: String,
}

/// Handle Artifactory-style artifact search API
/// GET /artifactory/api/search/artifact?name={name}&repos={repository}
async fn search_artifacts(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    Extension(base_url): Extension<String>,
) -> impl IntoResponse {
    let name = params.get("name").cloned().unwrap_or_default();
    let _repos = params.get("repos").cloned().unwrap_or_default();

    tracing::info!("Searching for artifacts matching name: {}", name);

    let state = state.read().unwrap();

    // Find all packages matching the name pattern
    let results: Vec<ArtifactSearchResult> = state
        .keys()
        .filter(|path| {
            // Path format: registry/{repository}/{name}/{version}/{name}-{version}.tgz
            // Extract the package name from the path and compare
            path.contains(&format!("/{name}/")) || path.contains(&format!("/{name}-"))
        })
        .map(|path| ArtifactSearchResult {
            uri: format!("{base_url}/{path}"),
        })
        .collect();

    tracing::info!("Found {} matching artifacts", results.len());

    (
        [(
            header::CONTENT_TYPE,
            "application/vnd.org.jfrog.artifactory.search.ArtifactSearchResult+json",
        )],
        Json(ArtifactSearchResponse { results }),
    )
}

// basic handler that responds with a static string
async fn get_package(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    tracing::info!("Downloaded package from {path}");
    let content = state
        .read()
        .unwrap()
        .get(&path)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(([(header::CONTENT_TYPE, "application/x-gzip")], content))
}

/// HEAD handler: returns 200 if artifact exists, 404 otherwise (no body)
async fn head_package(State(state): State<AppState>, Path(path): Path<String>) -> StatusCode {
    tracing::info!("HEAD check for {path}");
    if state.read().unwrap().contains_key(&path) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn put_package(State(state): State<AppState>, Path(path): Path<String>, body: Bytes) {
    tracing::info!("Uploaded package to {path} ({} bytes)", body.len());
    state.write().unwrap().insert(path, body);
}

// do not use (flavor = "current_thread") here because the user-provided function is blocking
#[tokio::main]
pub async fn with_test_registry<F: FnOnce(&str)>(f: F) {
    // spawn test registry in separate Tokio task
    let listen = SocketAddr::new("127.0.0.1".parse().unwrap(), 0);
    let listener = TcpListener::bind(listen).await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let base_url = format!("http://{local_addr}");
    let handle = tokio::task::spawn(test_registry(listener, base_url.clone()));

    tracing::info!("Listening on {local_addr:?}");
    let url = format!("{base_url}/registry");

    // wait until the test registry is ready
    let dur = Duration::from_millis(10);
    let client = reqwest::Client::builder()
        .connect_timeout(dur)
        .build()
        .unwrap();
    loop {
        // perform a simple request at an arbitrary URL to check readiness
        if client.get(&url).send().await.is_ok() {
            break;
        }
        // check whether the test registry has failed instead of looping indefinitely
        assert!(!handle.is_finished(), "test registry ended unexpectedly");
        // no busy wait
        tokio::time::sleep(dur).await;
    }

    // run user code
    f(&url);
}
