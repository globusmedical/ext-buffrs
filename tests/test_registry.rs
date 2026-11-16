use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use bytes::Bytes;
use miette::{miette, Context as _, IntoDiagnostic};
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};

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
    /// Number of upcoming connections to close before reading a request from them.
    dropped_connections: AtomicUsize,
    /// Number of upcoming GET requests to leave unanswered for [`STALL`].
    stalled: AtomicUsize,
    /// Number of upcoming package downloads whose connection is closed part way through the body.
    cut_off_downloads: AtomicUsize,
    /// Number of upcoming package downloads that stall for [`STALL`] part way through the body.
    stalled_downloads: AtomicUsize,
    /// Number of connections accepted so far.
    connections: AtomicUsize,
}

/// How long a stalled request stays unanswered.
const STALL: Duration = Duration::from_secs(5);

/// Decrements the counter unless it is zero, and says whether it did.
fn take_one(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

impl RegistryProbe {
    /// Paths of the GET requests received since the last call to [`Self::clear`].
    pub fn get_requests(&self) -> Vec<String> {
        self.get_requests.lock().unwrap().clone()
    }

    /// Forgets the recorded requests and connections.
    pub fn clear(&self) {
        self.get_requests.lock().unwrap().clear();
        self.connections.store(0, Ordering::SeqCst);
    }

    /// Number of connections the registry accepted since the last call to [`Self::clear`].
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
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

    /// Closes the next `count` connections to the registry as soon as they are accepted.
    pub fn drop_next_connections(&self, count: usize) {
        self.dropped_connections.store(count, Ordering::SeqCst);
    }

    /// Leaves the next `count` GET requests unanswered for several seconds.
    pub fn stall_next(&self, count: usize) {
        self.stalled.store(count, Ordering::SeqCst);
    }

    /// Sends the headers and half the body of the next `count` package downloads, then closes
    /// their connections.
    pub fn cut_off_next_downloads(&self, count: usize) {
        self.cut_off_downloads.store(count, Ordering::SeqCst);
    }

    /// Sends the headers and half the body of the next `count` package downloads, then stays
    /// silent for several seconds before closing their connections.
    pub fn stall_next_downloads(&self, count: usize) {
        self.stalled_downloads.store(count, Ordering::SeqCst);
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

    if take_one(&probe.stalled) {
        tokio::time::sleep(STALL).await;
    }

    let throttled = take_one(&probe.throttled)
        || probe
            .throttled_paths
            .lock()
            .unwrap()
            .iter()
            .any(|fragment| path.contains(fragment));
    if throttled {
        // The body is larger than what a client reads along with the headers.
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "0")],
            "slow down
"
            .repeat(3200),
        )
            .into_response();
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
    let Some(content) = state.read().unwrap().get(&path).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let interruption = if take_one(&probe.cut_off_downloads) {
        Some(Duration::from_millis(100))
    } else if take_one(&probe.stalled_downloads) {
        Some(STALL)
    } else {
        None
    };

    match interruption {
        Some(pause) => (
            [
                (header::CONTENT_TYPE, "application/x-gzip".to_owned()),
                (header::CONTENT_LENGTH, content.len().to_string()),
            ],
            interrupted_body(content, pause),
        )
            .into_response(),
        None => ([(header::CONTENT_TYPE, "application/x-gzip")], content).into_response(),
    }
}

/// A body that sends the first half of `content`, pauses for `pause` and then fails, which
/// closes the connection before the length announced in the headers has been sent.
fn interrupted_body(content: Bytes, pause: Duration) -> Body {
    let half = content.slice(..content.len() / 2);
    let parts = futures_util::stream::unfold(0, move |part| {
        let half = half.clone();
        async move {
            match part {
                0 => Some((Ok(half), 1)),
                1 => {
                    // The pause lets the server send the first half before the connection closes.
                    tokio::time::sleep(pause).await;
                    let error = std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "the test registry interrupts the body",
                    );
                    Some((Err(error), 2))
                }
                _ => None,
            }
        }
    });

    Body::from_stream(parts)
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

/// Accepts the connections of the tests and forwards them to the registry at `registry`, except
/// for those the probe asks to drop.
async fn forward_connections(
    listener: TcpListener,
    registry: SocketAddr,
    probe: Arc<RegistryProbe>,
) {
    while let Ok((mut inbound, _)) = listener.accept().await {
        probe.connections.fetch_add(1, Ordering::SeqCst);

        if take_one(&probe.dropped_connections) {
            continue;
        }

        tokio::spawn(async move {
            if let Ok(mut outbound) = TcpStream::connect(registry).await {
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            }
        });
    }
}

async fn run_with_test_registry<F: FnOnce(&str, &RegistryProbe)>(f: F) {
    // spawn test registry in separate Tokio task, behind a listener that can drop connections
    let listen = SocketAddr::new("127.0.0.1".parse().unwrap(), 0);
    let registry_listener = TcpListener::bind(listen).await.unwrap();
    let registry_addr = registry_listener.local_addr().unwrap();
    let listener = TcpListener::bind(listen).await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let base_url = format!("http://{local_addr}");
    let probe = Arc::new(RegistryProbe::default());
    let handle = tokio::task::spawn(test_registry(
        registry_listener,
        base_url.clone(),
        Arc::clone(&probe),
    ));
    tokio::task::spawn(forward_connections(
        listener,
        registry_addr,
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
