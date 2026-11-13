use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use bytes::Bytes;
use miette::{miette, Context as _, IntoDiagnostic};
use serde::Serialize;
use tokio::net::TcpListener;

type AppState = Arc<RwLock<HashMap<String, Bytes>>>;

/// What a test can observe and control on the test registry.
#[derive(Default)]
pub struct RegistryProbe {
    /// Paths of the GET requests served so far, in order.
    get_requests: Mutex<Vec<String>>,
    /// Number of upcoming GET requests to reject with 429.
    throttled: AtomicUsize,
    /// Path fragments whose GET requests are always rejected with 429.
    throttled_paths: Mutex<Vec<String>>,
    /// Paths the registry pretends not to have.
    hidden: Mutex<Vec<String>>,
}

impl RegistryProbe {
    /// Paths of the GET requests received since the last call to [`Self::clear`].
    pub fn get_requests(&self) -> Vec<String> {
        self.get_requests.lock().unwrap().clone()
    }

    /// Forgets the recorded requests.
    pub fn clear(&self) {
        self.get_requests.lock().unwrap().clear();
    }

    /// Rejects the next `count` GET requests with `429 Too Many Requests`.
    pub fn throttle_next(&self, count: usize) {
        self.throttled.store(count, Ordering::SeqCst);
    }

    /// Rejects every GET request whose path contains `fragment` with `429 Too Many Requests`.
    pub fn throttle_path(&self, fragment: &str) {
        self.throttled_paths
            .lock()
            .unwrap()
            .push(fragment.to_owned());
    }

    /// Makes the registry answer as if the artifact at `path` had been deleted.
    pub fn hide(&self, path: &str) {
        self.hidden.lock().unwrap().push(path.to_owned());
    }

    fn is_hidden(&self, path: &str) -> bool {
        self.hidden
            .lock()
            .unwrap()
            .iter()
            .any(|hidden| hidden == path)
    }
}

/// Run a minimal registry for local testing
async fn test_registry(
    listener: TcpListener,
    base_url: String,
    probe: Arc<RegistryProbe>,
) -> miette::Result<()> {
    let state = Arc::new(RwLock::new(HashMap::<String, Bytes>::new()));
    let app = Router::new()
        .route("/artifactory/api/search/artifact", get(search_artifacts))
        .route(
            "/*path",
            get(get_package).head(head_package).put(put_package),
        )
        .with_state(state)
        .layer(Extension(base_url))
        .layer(Extension(probe));
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

/// Serves a package, or the Artifactory storage listing of a package folder for
/// `GET {registry}/api/storage/{repository}/{name}/`.
async fn get_package(
    State(state): State<AppState>,
    Extension(probe): Extension<Arc<RegistryProbe>>,
    Path(path): Path<String>,
) -> Response {
    probe.get_requests.lock().unwrap().push(path.clone());

    let throttled = probe
        .throttled
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
        || probe
            .throttled_paths
            .lock()
            .unwrap()
            .iter()
            .any(|fragment| path.contains(fragment));
    if throttled {
        return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "0")]).into_response();
    }

    if let Some((registry, folder)) = path.split_once("/api/storage/") {
        let prefix = format!("{registry}/{folder}");
        let children: Vec<StorageChild> = state
            .read()
            .unwrap()
            .keys()
            .filter(|key| !probe.is_hidden(key))
            .filter_map(|key| key.strip_prefix(&prefix))
            .map(|file| StorageChild {
                uri: format!("/{file}"),
                folder: false,
            })
            .collect();

        if children.is_empty() {
            return StatusCode::NOT_FOUND.into_response();
        }

        return Json(StorageFolder { children }).into_response();
    }

    if probe.is_hidden(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    tracing::info!("Downloaded package from {path}");
    match state.read().unwrap().get(&path).cloned() {
        Some(content) => ([(header::CONTENT_TYPE, "application/x-gzip")], content).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Artifactory storage API folder listing
#[derive(Serialize)]
struct StorageFolder {
    children: Vec<StorageChild>,
}

#[derive(Serialize)]
struct StorageChild {
    uri: String,
    folder: bool,
}

/// HEAD handler: returns 200 if artifact exists, 404 otherwise (no body)
async fn head_package(
    State(state): State<AppState>,
    Extension(probe): Extension<Arc<RegistryProbe>>,
    Path(path): Path<String>,
) -> StatusCode {
    tracing::info!("HEAD check for {path}");
    if state.read().unwrap().contains_key(&path) && !probe.is_hidden(&path) {
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
    run_with_test_registry(|url, _| f(url)).await;
}

/// Like [`with_test_registry`], and also hands the test a [`RegistryProbe`].
// do not use (flavor = "current_thread") here because the user-provided function is blocking
#[tokio::main]
pub async fn with_probed_test_registry<F: FnOnce(&str, &RegistryProbe)>(f: F) {
    run_with_test_registry(f).await;
}

async fn run_with_test_registry<F: FnOnce(&str, &RegistryProbe)>(f: F) {
    // spawn test registry in separate Tokio task
    let listen = SocketAddr::new("127.0.0.1".parse().unwrap(), 0);
    let listener = TcpListener::bind(listen).await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let base_url = format!("http://{local_addr}");
    let probe = Arc::new(RegistryProbe::default());
    let handle = tokio::task::spawn(test_registry(
        listener,
        base_url.clone(),
        Arc::clone(&probe),
    ));

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

    // the readiness probe above is not part of what a test observes
    probe.clear();

    // run user code
    f(&url, &probe);
}
