// Copyright 2023 Helsing GmbH
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{
    credentials::Credentials,
    manifest::{Dependency, DependencyManifest},
    package::{Package, PackageName},
    registry::RegistryUri,
};
use bytes::Bytes;
use miette::{ensure, miette, Context, IntoDiagnostic};
use reqwest::{header::HeaderMap, Body, Method, Response, StatusCode};
use semver::Version;
use serde::Deserialize;
use std::time::{Duration, SystemTime};
use url::Url;

/// The policy for validating artifactory server certificates
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CertValidationPolicy {
    /// Validate the certificate
    #[default]
    Validate,

    /// Do not validate the certificate
    NoValidation,
}

/// Environment variable for specifying a custom CA bundle file
pub const ENV_CA_BUNDLE: &str = "BUFFRS_CA_BUNDLE";

/// Builds a configured `reqwest::Client` for Artifactory operations.
///
/// This helper ensures consistent TLS and redirect configuration across all
/// Artifactory clients. Use this when creating a shared client to be passed
/// to multiple `Artifactory::new_with_client` calls.
///
/// # CA Certificate Configuration
///
/// The client can be configured to use a custom CA certificate bundle by
/// setting the `BUFFRS_CA_BUNDLE` environment variable to the path of a
/// PEM-encoded certificate file. This is useful for:
/// - Corporate environments with internal CAs
/// - Self-signed certificates
/// - Custom PKI setups
///
/// If `BUFFRS_CA_BUNDLE` is set but the file cannot be read, an error is returned.
///
/// # Timeouts
///
/// Connecting to the registry is given up after 30 seconds. A request fails when the registry
/// sends nothing for 60 seconds, counted from the start of the request until the response
/// headers arrive and between two reads of the response body. A download of any size therefore
/// continues for as long as data arrives, while an upload has to be answered within the limit.
///
/// `BUFFRS_HTTP_CONNECT_TIMEOUT` and `BUFFRS_HTTP_TIMEOUT` override the two limits with a number
/// of seconds, and `0` removes the limit. Any other value is an error.
pub fn build_reqwest_client(policy: CertValidationPolicy) -> miette::Result<reqwest::Client> {
    let variable = |name: &str| std::env::var(name).ok();

    let timeouts = HttpTimeouts {
        connect: timeout_setting(
            ENV_HTTP_CONNECT_TIMEOUT,
            variable(ENV_HTTP_CONNECT_TIMEOUT).as_deref(),
            DEFAULT_CONNECT_TIMEOUT,
        )?,
        read: timeout_setting(
            ENV_HTTP_TIMEOUT,
            variable(ENV_HTTP_TIMEOUT).as_deref(),
            DEFAULT_READ_TIMEOUT,
        )?,
    };

    build_client(policy, timeouts)
}

/// Environment variable for the connect timeout of registry requests, in seconds
const ENV_HTTP_CONNECT_TIMEOUT: &str = "BUFFRS_HTTP_CONNECT_TIMEOUT";

/// Environment variable for the read timeout of registry requests, in seconds
const ENV_HTTP_TIMEOUT: &str = "BUFFRS_HTTP_TIMEOUT";

/// Time allowed for connecting to the registry unless the environment overrides it.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Time the registry may stay silent during a request unless the environment overrides it.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The time limits of a registry client. `None` is no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HttpTimeouts {
    /// Time allowed for establishing a connection.
    connect: Option<Duration>,
    /// Time allowed for the response headers to arrive, and between two reads of the body.
    read: Option<Duration>,
}

/// The timeout that the environment variable `name` with the given value selects.
///
/// An unset or empty variable selects `default`, `0` selects no timeout, and any other number is
/// a timeout in seconds.
fn timeout_setting(
    name: &str,
    value: Option<&str>,
    default: Duration,
) -> miette::Result<Option<Duration>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Some(default));
    };

    let seconds: u64 = value.parse().into_diagnostic().wrap_err(miette!(
        "{name} must be a number of seconds, got \"{value}\""
    ))?;

    Ok((seconds > 0).then(|| Duration::from_secs(seconds)))
}

fn build_client(
    policy: CertValidationPolicy,
    timeouts: HttpTimeouts,
) -> miette::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(policy == CertValidationPolicy::NoValidation);

    if let Some(timeout) = timeouts.connect {
        builder = builder.connect_timeout(timeout);
    }

    if let Some(timeout) = timeouts.read {
        builder = builder.read_timeout(timeout);
    }

    // Check for custom CA bundle from environment variable
    if let Ok(ca_bundle_path) = std::env::var(ENV_CA_BUNDLE) {
        if !ca_bundle_path.is_empty() {
            let ca_bundle_path = std::path::Path::new(&ca_bundle_path);
            let cert_pem = std::fs::read(ca_bundle_path)
                .into_diagnostic()
                .wrap_err(miette!(
                    "failed to read CA bundle from {} (set via {})",
                    ca_bundle_path.display(),
                    ENV_CA_BUNDLE
                ))?;

            // Parse the PEM file which may contain multiple certificates
            for cert in reqwest::Certificate::from_pem_bundle(&cert_pem)
                .into_diagnostic()
                .wrap_err(miette!(
                    "failed to parse CA certificates from {}",
                    ca_bundle_path.display()
                ))?
            {
                builder = builder.add_root_certificate(cert);
            }

            tracing::debug!(
                "loaded custom CA bundle from {} ({})",
                ca_bundle_path.display(),
                ENV_CA_BUNDLE
            );
        }
    }

    builder.build().into_diagnostic()
}

/// The registry implementation for artifactory
#[derive(Debug, Clone)]
pub struct Artifactory {
    registry: RegistryUri,
    token: Option<String>,
    client: reqwest::Client,
}

impl Artifactory {
    /// Creates a new instance of an Artifactory registry client
    ///
    /// # Arguments
    /// * `registry` - The registry URI
    /// * `credentials` - The credentials to use for the registry
    /// * `policy` - The policy for validating artifactory server certificates
    pub fn new(
        registry: RegistryUri,
        credentials: &Credentials,
        policy: CertValidationPolicy,
    ) -> miette::Result<Self> {
        let client = build_reqwest_client(policy)?;
        Self::new_with_client(registry, credentials, client)
    }

    /// Creates a new instance with a pre-built `reqwest::Client`.
    ///
    /// Use this when you want to share a connection pool across multiple
    /// Artifactory clients within the same invocation.
    ///
    /// # Arguments
    /// * `registry` - The registry URI
    /// * `credentials` - The credentials to use for the registry
    /// * `client` - A pre-configured `reqwest::Client` (use `build_reqwest_client`)
    pub fn new_with_client(
        registry: RegistryUri,
        credentials: &Credentials,
        client: reqwest::Client,
    ) -> miette::Result<Self> {
        let token = credentials.registry_tokens.get(&registry).cloned();
        Ok(Self {
            registry,
            token,
            client,
        })
    }

    fn new_request(&self, method: Method, url: Url) -> RequestBuilder {
        let mut request_builder = RequestBuilder::new(&self.client, method, url);

        if let Some(token) = &self.token {
            request_builder = request_builder.auth(token.clone());
        }

        request_builder
    }

    /// Pings artifactory to ensure registry access is working
    pub async fn ping(&self) -> miette::Result<()> {
        let repositories_url: Url = {
            let mut uri: url::Url = self.registry.clone().into();
            let path = &format!("{}/api/repositories", uri.path());
            uri.set_path(path);
            uri
        };

        self.new_request(Method::GET, repositories_url)
            .send()
            .await
            .map(|_| ())
    }

    /// Retrieves all available versions of a package from artifactory.
    ///
    /// Uses the Storage API (`/api/storage/{repo}/{name}/`) which supports
    /// both local and virtual repositories, unlike the Artifact Search API.
    pub async fn list_versions(
        &self,
        repository: String,
        name: PackageName,
    ) -> miette::Result<Vec<Version>> {
        let storage_url: Url = {
            let mut uri: url::Url = self.registry.clone().into();
            let base = uri.path().trim_end_matches('/');
            uri.set_path(&format!("{base}/api/storage/{repository}/{name}/"));
            uri
        };

        tracing::debug!("Listing versions via storage API: {storage_url}");

        let response = self
            .new_request(Method::GET, storage_url.clone())
            .send_and_read()
            .await?;

        // A 404 means the package folder doesn't exist yet — return empty
        if response.status == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!("Package folder not found at {storage_url}, returning empty versions");
            return Ok(Vec::new());
        }

        if response.status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(miette!(
                "unauthorized - please provide registry credentials with `buffrs login`"
            ));
        }

        if !response.status.is_success() {
            return Err(miette!(
                "unexpected status {} from storage API at {storage_url}",
                response.status
            ));
        }

        let parsed_response = serde_json::from_slice::<StorageFolderInfo>(&response.body)
            .into_diagnostic()
            .wrap_err(miette!(
                "unexpected error: response could not be deserialized to StorageFolderInfo"
            ))?;

        tracing::debug!(
            "Storage folder listing for {}/{}: {} children",
            repository,
            name,
            parsed_response.children.len()
        );

        let name_str = name.to_string();
        let versions: Vec<Version> = parsed_response
            .children
            .iter()
            .filter(|child| !child.folder)
            .filter_map(|child| {
                let filename = child.uri.trim_start_matches('/');
                let artifact_name = filename.strip_suffix(".tgz")?;
                let version_str = artifact_name
                    .strip_prefix(&name_str)
                    .and_then(|rest| rest.strip_prefix('-'))?;
                let version = Version::parse(version_str).ok()?;
                // Round-trip check: ensure the parsed version reconstructs the filename
                let expected = format!("{name}-{version}");
                if expected == artifact_name {
                    Some(version)
                } else {
                    None
                }
            })
            .collect();

        tracing::debug!(
            "Found {} versions for {}: {:?}",
            versions.len(),
            name,
            versions
        );
        Ok(versions)
    }

    /// Retrieves the latest version of a package by querying artifactory. Returns an error if no artifact could be found
    pub async fn get_latest_version(
        &self,
        repository: String,
        name: PackageName,
    ) -> miette::Result<Version> {
        let versions = self.list_versions(repository, name.clone()).await?;

        versions.into_iter().max().ok_or_else(|| {
            miette!("no version could be found on artifactory for this artifact name. Does it exist in this registry and repository?")
        })
    }

    /// Downloads a package from artifactory using an already-resolved exact version.
    ///
    /// This is the preferred method when version resolution has already been performed.
    pub async fn download_version(
        &self,
        repository: &str,
        name: &PackageName,
        version: &Version,
    ) -> miette::Result<Package> {
        self.try_download_version(repository, name, version)
            .await
            .map_err(DownloadFailure::into_report)
    }

    /// Like [`Self::download_version`], and tells a missing artifact and a throttled request
    /// apart from every other failure.
    pub(crate) async fn try_download_version(
        &self,
        repository: &str,
        name: &PackageName,
        version: &Version,
    ) -> Result<Package, DownloadFailure> {
        use crate::version::version_to_artifact_string;

        let artifact_url = {
            let version_str = version_to_artifact_string(version);
            let url: RegistryUri = self.registry.clone();
            let mut url: url::Url = url.into();
            let path = url.path();

            url.set_path(&format!(
                "{path}/{repository}/{name}/{name}-{version_str}.tgz"
            ));

            url
        };

        tracing::debug!("Hitting download URL: {artifact_url}");

        let response = self
            .new_request(Method::GET, artifact_url)
            .send_and_read()
            .await
            .map_err(DownloadFailure::Other)?;
        let status = response.status;
        let response = match response.validate() {
            Ok(response) => response,
            Err(e) if status == StatusCode::NOT_FOUND || status == StatusCode::GONE => {
                return Err(DownloadFailure::NotFound(e))
            }
            Err(e) if is_throttled(status) => return Err(DownloadFailure::Throttled(e)),
            Err(e) => return Err(DownloadFailure::Other(e)),
        };

        let content_type = response
            .headers
            .get(&reqwest::header::CONTENT_TYPE)
            .ok_or_else(|| DownloadFailure::Other(miette!("missing content-type header")))?;

        if content_type != reqwest::header::HeaderValue::from_static("application/x-gzip") {
            return Err(DownloadFailure::Other(miette!(
                "server response has incorrect mime type: {content_type:?}"
            )));
        }

        Package::try_from(response.body)
            .wrap_err(miette!(
                "failed to download dependency {}@{}",
                name,
                version
            ))
            .map_err(DownloadFailure::Other)
    }

    /// Downloads a package from artifactory.
    ///
    /// Note: This method requires the dependency to have an exact version pinned.
    /// For flexible version requirements, use `download_version()` after resolving
    /// the version with `list_versions()` and `select_version()`.
    pub async fn download(&self, dependency: Dependency) -> miette::Result<Package> {
        let DependencyManifest::Remote(ref manifest) = dependency.manifest else {
            return Err(miette!(
                "unable to download local dependency ({}) from artifactory",
                dependency.package
            ));
        };

        let artifact_url = {
            let version = super::dependency_version_string(&dependency)?;
            let url: RegistryUri = self.registry.clone();
            let mut url: url::Url = url.into();
            let path = url.path();

            url.set_path(&format!(
                "{}/{}/{}/{}-{}.tgz",
                path, manifest.repository, dependency.package, dependency.package, version
            ));

            url
        };

        tracing::debug!("Hitting download URL: {artifact_url}");

        let response = self
            .new_request(Method::GET, artifact_url)
            .send_and_read()
            .await?
            .validate()?;
        let content_type = response
            .headers
            .get(&reqwest::header::CONTENT_TYPE)
            .ok_or_else(|| miette!("missing content-type header"))?;

        ensure!(
            content_type == reqwest::header::HeaderValue::from_static("application/x-gzip"),
            "server response has incorrect mime type: {content_type:?}"
        );

        Package::try_from(response.body).wrap_err(miette!(
            "failed to download dependency {}",
            dependency.package
        ))
    }

    /// Publishes a package to artifactory
    pub async fn publish(&self, package: Package, repository: String) -> miette::Result<()> {
        let local_deps: Vec<&Dependency> = package
            .manifest
            .dependencies
            .iter()
            .filter(|d| d.manifest.is_local())
            .collect();

        // abort publishing if we have local dependencies
        if !local_deps.is_empty() {
            let names: Vec<String> = local_deps.iter().map(|d| d.package.to_string()).collect();

            return Err(miette!(
                "unable to publish {} to artifactory due having the following local dependencies: {}",
                package.name(),
                names.join(", ")
            ));
        }

        let artifact_uri: Url = format!(
            "{}/{}/{}/{}-{}.tgz",
            self.registry,
            repository,
            package.name(),
            package.name(),
            package.version(),
        )
        .parse()
        .into_diagnostic()
        .wrap_err(miette!(
            "unexpected error: failed to construct artifact URL"
        ))?;

        // Check if artifact already exists (idempotent publish)
        let check_response = self
            .new_request(Method::HEAD, artifact_uri.clone())
            .send_raw()
            .await?;

        let status = check_response.status();
        if status.is_success() {
            // Artifact already exists - skip upload
            tracing::info!(
                ":: skipped {}/{}@{} (already published)",
                repository,
                package.name(),
                package.version()
            );
            return Ok(());
        } else if status == reqwest::StatusCode::NOT_FOUND {
            // Artifact does not exist - proceed with upload
        } else if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(miette!(
                "unauthorized - please provide registry credentials with `buffrs login`"
            ));
        } else {
            // Unexpected status
            return Err(miette!(
                "unexpected status {} when checking if artifact exists at {}",
                status,
                artifact_uri
            ));
        }

        let _ = self
            .new_request(Method::PUT, artifact_uri)
            .body(package.tgz.clone())
            .send()
            .await?;

        tracing::info!(
            ":: published {}/{}@{}",
            repository,
            package.name(),
            package.version()
        );

        Ok(())
    }
}

struct RequestBuilder {
    request: reqwest::RequestBuilder,
    method: Method,
}

impl RequestBuilder {
    fn new(client: &reqwest::Client, method: Method, url: Url) -> Self {
        Self {
            request: client.request(method.clone(), url),
            method,
        }
    }

    fn auth(mut self, token: String) -> Self {
        self.request = self.request.bearer_auth(token);
        self
    }

    fn body(mut self, payload: impl Into<Body>) -> Self {
        self.request = self.request.body(payload);
        self
    }

    /// Sends the request and fails unless the registry answered with success.
    async fn send(self) -> miette::Result<Response> {
        let response = self.send_with_retry().await?;
        check_status(response.status(), response.url())?;
        Ok(response)
    }

    /// Send request and return raw response without validation.
    /// Used for checking artifact existence where 404 is an expected outcome.
    async fn send_raw(self) -> miette::Result<reqwest::Response> {
        self.send_with_retry().await
    }

    /// Sends the request, waiting and resending while the registry throttles it or cannot be
    /// reached, and returns the response once its headers arrive.
    ///
    /// A registry behind a rate limiter answers 429 (or 503 while overloaded) to a burst of
    /// requests, which several concurrent installs from one address produce, and a busy registry
    /// or network drops or stalls connections. The request is sent up to [`MAX_SEND_ATTEMPTS`]
    /// times in total, and not again once [`MAX_TOTAL_RETRY_WAIT`] has passed since it was first
    /// sent; after that the last response or error is returned as is. [`is_retryable`] decides
    /// which failures are resent. A request whose body cannot be replayed is sent once.
    async fn send_with_retry(self) -> miette::Result<reqwest::Response> {
        self.retry(|response| async move { Ok(response) })
            .await
            .into_diagnostic()
    }

    /// Like [`Self::send_with_retry`], and reads the whole response body before it returns.
    ///
    /// A failure while the body is read is resent like one before the response arrived, so a
    /// download that the registry or the network cuts off or stalls part way is sent again.
    async fn send_and_read(self) -> miette::Result<FetchedResponse> {
        let fetched = self
            .retry(|response| async move {
                let status = response.status();
                let headers = response.headers().clone();
                let url = response.url().clone();
                let body = response.bytes().await?;

                Ok(FetchedResponse {
                    status,
                    headers,
                    url,
                    body,
                })
            })
            .await;

        match fetched {
            Ok(fetched) => Ok(fetched),
            Err(error) if error.is_body() || error.is_decode() => {
                Err(error).into_diagnostic().wrap_err(miette!(
                    "unexpected error: unable to retrieve response payload"
                ))
            }
            Err(error) => Err(error).into_diagnostic(),
        }
    }

    /// The retry loop of [`Self::send_with_retry`] and [`Self::send_and_read`].
    ///
    /// `receive` turns a response that is not throttled into the result, and a failure it
    /// returns is resent like one before the response arrived. A throttled response that is not
    /// resent any more is passed to `receive` as well.
    async fn retry<T, F, Fut>(self, receive: F) -> Result<T, reqwest::Error>
    where
        F: Fn(Response) -> Fut,
        Fut: std::future::Future<Output = Result<T, reqwest::Error>>,
    {
        let started = tokio::time::Instant::now();
        let mut attempt = 1;

        loop {
            let Some(request) = self.request.try_clone() else {
                return receive(self.request.send().await?).await;
            };

            let within_budget = |delay: Option<Duration>| {
                delay.filter(|delay| {
                    attempt < MAX_SEND_ATTEMPTS
                        && started.elapsed() + *delay <= MAX_TOTAL_RETRY_WAIT
                })
            };

            let error = match request.send().await {
                Ok(response) if is_throttled(response.status()) => {
                    let Some(delay) = within_budget(retry_delay(response.headers(), attempt))
                    else {
                        return receive(response).await;
                    };
                    let resend_at = tokio::time::Instant::now() + delay;

                    tracing::warn!(
                        "registry answered {} for {}, retrying in {:.1}s (attempt {attempt} of {MAX_SEND_ATTEMPTS})",
                        response.status(),
                        response.url(),
                        delay.as_secs_f64()
                    );

                    // Reading the body to its end lets the connection be used again.
                    let _ = tokio::time::timeout_at(resend_at, drain(response)).await;

                    tokio::time::sleep_until(resend_at).await;
                    attempt += 1;
                    continue;
                }
                Ok(response) => match receive(response).await {
                    Ok(received) => return Ok(received),
                    Err(error) => error,
                },
                Err(error) => error,
            };

            let delay = is_retryable(&self.method, &error)
                .then(|| retry_delay(&HeaderMap::new(), attempt))
                .flatten();
            let Some(delay) = within_budget(delay) else {
                return Err(error);
            };

            tracing::warn!(
                "{error} ({}), retrying in {:.1}s (attempt {attempt} of {MAX_SEND_ATTEMPTS})",
                root_cause(&error),
                delay.as_secs_f64()
            );

            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }
}

/// A response whose body has been read in full.
struct FetchedResponse {
    status: StatusCode,
    headers: HeaderMap,
    url: Url,
    body: Bytes,
}

impl FetchedResponse {
    /// Fails unless the registry answered with success, as [`check_status`] decides.
    fn validate(self) -> miette::Result<Self> {
        check_status(self.status, &self.url)?;
        Ok(self)
    }
}

/// Fails for a redirect, a missing authorisation and an error status of a response from `url`.
///
/// An error status gets the text of the error that `reqwest::Response::error_for_status`
/// returns.
fn check_status(status: StatusCode, url: &Url) -> miette::Result<()> {
    ensure!(
        !status.is_redirection(),
        "remote server attempted to redirect request - is this registry URL valid?"
    );

    ensure!(
        status != StatusCode::UNAUTHORIZED,
        "unauthorized - please provide registry credentials with `buffrs login`"
    );

    let kind = if status.is_client_error() {
        "client"
    } else if status.is_server_error() {
        "server"
    } else {
        return Ok(());
    };

    Err(miette!(
        "HTTP status {kind} error ({status}) for url ({url})"
    ))
}

/// Total number of times a request is sent before its response or error is returned as is.
const MAX_SEND_ATTEMPTS: u32 = 6;

/// Longest `Retry-After` the client waits for. A longer cooldown ends the retries.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Shortest wait before a resend, used when the registry sends `Retry-After: 0`.
const MIN_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Longest time after the first attempt of a request at which it is still sent again.
const MAX_TOTAL_RETRY_WAIT: Duration = Duration::from_secs(120);

/// Largest body of a throttled response that is read so that its connection can be used again.
const MAX_DRAINED_BODY: usize = 64 * 1024;

/// Whether the status asks the client to slow down and try again.
fn is_throttled(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
}

/// Whether a request that failed with `error` before its response was read may be sent again.
///
/// A request that never reached the registry, because no connection could be established, is
/// sent again whatever its method. Any other failure, such as a timeout or a connection that
/// the registry closed before or while sending the response, is only resent for a method
/// without side effects: the registry may have stored an upload whose response was lost.
///
/// A response body that breaks off is a decode error whose source is a body error, because the
/// client reads it through its content decoder.
fn is_retryable(method: &Method, error: &reqwest::Error) -> bool {
    let interrupted =
        error.is_timeout() || error.is_request() || error.is_body() || error.is_decode();

    error.is_connect() || (method.is_safe() && interrupted)
}

/// The innermost cause of the error, which names what went wrong on the connection.
fn root_cause(error: &reqwest::Error) -> &(dyn std::error::Error + 'static) {
    let mut cause: &(dyn std::error::Error + 'static) = error;

    while let Some(source) = cause.source() {
        cause = source;
    }

    cause
}

/// Reads the response body to its end, or gives up after [`MAX_DRAINED_BODY`] bytes.
async fn drain(mut response: Response) {
    let mut remaining = MAX_DRAINED_BODY;

    while let Ok(Some(chunk)) = response.chunk().await {
        let Some(rest) = remaining.checked_sub(chunk.len()) else {
            return;
        };
        remaining = rest;
    }
}

/// Time to wait before resending after the given attempt (1-based).
///
/// A `Retry-After` header, given in seconds or as an HTTP date, is never shortened, so the
/// request is not resent before the cooldown the registry asked for. `None` means that cooldown
/// exceeds [`MAX_RETRY_AFTER`] and the request should not be resent. Without a usable header the
/// wait doubles from one second per attempt. Every wait gets up to 50% jitter on top so that
/// concurrent installs spread out.
fn retry_delay(headers: &reqwest::header::HeaderMap, attempt: u32) -> Option<Duration> {
    retry_delay_at(headers, attempt, SystemTime::now())
}

/// [`retry_delay`] with `now` as the time an HTTP date in `Retry-After` is measured from.
fn retry_delay_at(
    headers: &reqwest::header::HeaderMap,
    attempt: u32,
    now: SystemTime,
) -> Option<Duration> {
    let retry_after = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value.trim(), now));

    let base = match retry_after {
        Some(retry_after) if retry_after > MAX_RETRY_AFTER => return None,
        Some(retry_after) => retry_after.max(MIN_RETRY_DELAY),
        None => Duration::from_secs(1) * 2u32.saturating_pow(attempt.saturating_sub(1)),
    };

    Some(base + base.mul_f64(0.5 * jitter_fraction()))
}

/// The cooldown that a `Retry-After` value asks for at the time `now`.
///
/// The value is a number of seconds or an HTTP date. A date that has passed is a cooldown of
/// zero. `None` means the value is neither.
fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let date = httpdate::parse_http_date(value).ok()?;

    Some(date.duration_since(now).unwrap_or(Duration::ZERO))
}

/// A pseudo-random value in `[0, 1)`, seeded per call by the standard library's hasher.
fn jitter_fraction() -> f64 {
    use std::hash::{BuildHasher, Hasher};

    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();

    (random % 1000) as f64 / 1000.0
}

/// Why a version download failed.
pub(crate) enum DownloadFailure {
    /// The registry does not have the artifact (404 or 410).
    NotFound(miette::Report),
    /// The registry still throttled the request after all retries.
    Throttled(miette::Report),
    /// Any other failure, such as missing credentials, a transport error or a broken archive.
    Other(miette::Report),
}

impl DownloadFailure {
    /// The underlying error.
    pub(crate) fn into_report(self) -> miette::Report {
        match self {
            Self::NotFound(report) | Self::Throttled(report) | Self::Other(report) => report,
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
struct StorageFolderInfo {
    children: Vec<StorageChild>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
struct StorageChild {
    uri: String,
    folder: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
    use reqwest::StatusCode;

    fn retry_after(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn retry_after_is_never_shortened() {
        let delay = retry_delay(&retry_after("45"), 1).unwrap();

        assert!(delay >= Duration::from_secs(45), "{delay:?}");
        assert!(delay <= Duration::from_millis(67_500), "{delay:?}");
    }

    #[test]
    fn retry_after_of_zero_still_waits() {
        let delay = retry_delay(&retry_after("0"), 1).unwrap();

        assert!(delay >= MIN_RETRY_DELAY, "{delay:?}");
        assert!(delay <= MIN_RETRY_DELAY.mul_f64(1.5), "{delay:?}");
    }

    #[test]
    fn retry_after_beyond_the_limit_ends_the_retries() {
        assert!(retry_delay(&retry_after("60"), 1).is_some());
        assert_eq!(retry_delay(&retry_after("61"), 1), None);
    }

    /// A fixed point in time, on a whole second as HTTP dates are.
    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn retry_after_date(date: SystemTime) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_str(&httpdate::fmt_http_date(date)).unwrap(),
        );
        headers
    }

    #[test]
    fn retry_after_as_a_future_date_waits_until_then() {
        let headers = retry_after_date(now() + Duration::from_secs(30));
        let delay = retry_delay_at(&headers, 1, now()).unwrap();

        assert!(delay >= Duration::from_secs(30), "{delay:?}");
        assert!(delay <= Duration::from_secs(45), "{delay:?}");
    }

    #[test]
    fn retry_after_as_a_past_date_waits_the_minimum() {
        let headers = retry_after_date(now() - Duration::from_secs(3600));
        let delay = retry_delay_at(&headers, 4, now()).unwrap();

        assert!(delay >= MIN_RETRY_DELAY, "{delay:?}");
        assert!(delay <= MIN_RETRY_DELAY.mul_f64(1.5), "{delay:?}");
    }

    #[test]
    fn retry_after_as_a_date_beyond_the_limit_ends_the_retries() {
        let at_limit = retry_after_date(now() + MAX_RETRY_AFTER);
        let beyond = retry_after_date(now() + MAX_RETRY_AFTER + Duration::from_secs(1));

        assert!(retry_delay_at(&at_limit, 1, now()).is_some());
        assert_eq!(retry_delay_at(&beyond, 1, now()), None);
    }

    #[test]
    fn backoff_doubles_without_a_usable_header() {
        for headers in [HeaderMap::new(), retry_after("soon")] {
            for attempt in 1..MAX_SEND_ATTEMPTS {
                let base = Duration::from_secs(1) * 2u32.pow(attempt - 1);
                let delay = retry_delay(&headers, attempt).unwrap();

                assert!(delay >= base, "attempt {attempt}: {delay:?}");
                assert!(delay <= base.mul_f64(1.5), "attempt {attempt}: {delay:?}");
            }
        }
    }

    #[test]
    fn only_429_and_503_are_throttling() {
        assert!(is_throttled(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_throttled(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_throttled(StatusCode::NOT_FOUND));
        assert!(!is_throttled(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn timeout_setting_reads_seconds() {
        let default = Duration::from_secs(60);
        let setting = |value| timeout_setting("BUFFRS_HTTP_TIMEOUT", value, default);

        assert_eq!(setting(None).unwrap(), Some(default));
        assert_eq!(setting(Some("")).unwrap(), Some(default));
        assert_eq!(setting(Some(" 5 ")).unwrap(), Some(Duration::from_secs(5)));
        assert_eq!(setting(Some("0")).unwrap(), None);

        let error = setting(Some("5s")).unwrap_err().to_string();
        assert!(error.contains("BUFFRS_HTTP_TIMEOUT"), "{error}");
        assert!(setting(Some("-1")).is_err());
    }

    /// Accepts connections and passes each to `serve`. Returns the URL to request.
    async fn serve_connections<F, Fut>(serve: F) -> String
    where
        F: Fn(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/artifact", listener.local_addr().unwrap());

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream));
            }
        });

        url
    }

    /// A client that gives up on a silent registry after a short time.
    fn impatient_client() -> reqwest::Client {
        build_client(
            CertValidationPolicy::Validate,
            HttpTimeouts {
                connect: Some(Duration::from_secs(5)),
                read: Some(Duration::from_millis(300)),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn request_to_a_silent_registry_times_out() {
        // The registry accepts the connection and never answers.
        let url = serve_connections(|stream| async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(stream);
        })
        .await;

        let started = std::time::Instant::now();
        let error = impatient_client().get(&url).send().await.unwrap_err();

        assert!(error.is_timeout(), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(is_retryable(&Method::GET, &error));
        assert!(is_retryable(&Method::HEAD, &error));
        assert!(!is_retryable(&Method::PUT, &error));
    }

    #[tokio::test]
    async fn closed_connection_is_retried_for_safe_methods_only() {
        // The registry closes every connection without answering.
        let url = serve_connections(|stream| async move { drop(stream) }).await;

        let error = impatient_client().get(&url).send().await.unwrap_err();

        assert!(!error.is_connect(), "{error:?}");
        assert!(is_retryable(&Method::GET, &error));
        assert!(!is_retryable(&Method::PUT, &error));
    }

    #[tokio::test]
    async fn failed_connect_is_retried_for_every_method() {
        // Nothing listens on the port of a listener that has been closed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/artifact", listener.local_addr().unwrap());
        drop(listener);

        let client = build_client(
            CertValidationPolicy::Validate,
            HttpTimeouts {
                connect: Some(Duration::from_secs(30)),
                read: None,
            },
        )
        .unwrap();
        let error = client.put(&url).send().await.unwrap_err();

        assert!(error.is_connect(), "{error:?}");
        assert!(is_retryable(&Method::PUT, &error));
    }

    /// The body of a response that the test registries below send in full.
    const BODY: &[u8] = &[b'x'; 4096];

    /// What a connection of [`interrupting_registry`] does after sending the response headers.
    #[derive(Clone, Copy)]
    enum Interruption {
        /// Sends part of the body and closes the connection.
        Cut,
        /// Sends part of the body and stays silent.
        Stall,
    }

    /// A registry that answers the first request with an interrupted body and later requests in
    /// full, each on a connection of its own. Returns the URL and the number of connections.
    async fn interrupting_registry(
        interruption: Interruption,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let connections = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&connections);
        let url = serve_connections(move |mut stream| {
            let first = counter.fetch_add(1, Ordering::SeqCst) == 0;
            async move {
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }

                let headers = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/x-gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    BODY.len()
                );
                let body = if first { &BODY[..BODY.len() / 2] } else { BODY };
                let _ = stream.write_all(headers.as_bytes()).await;
                let _ = stream.write_all(body).await;
                let _ = stream.flush().await;

                if first {
                    if let Interruption::Stall = interruption {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                }
            }
        })
        .await;

        (url, connections)
    }

    #[tokio::test]
    async fn body_cut_off_is_retried_for_safe_methods_only() {
        let (url, _) = interrupting_registry(Interruption::Cut).await;
        let error = impatient_client()
            .get(&url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap_err();

        assert!(is_retryable(&Method::GET, &error), "{error:?}");
        assert!(!is_retryable(&Method::PUT, &error), "{error:?}");

        let (url, connections) = interrupting_registry(Interruption::Cut).await;
        let response = RequestBuilder::new(&impatient_client(), Method::GET, url.parse().unwrap())
            .send_and_read()
            .await
            .unwrap();

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, BODY);
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stalled_body_times_out_and_is_retried() {
        let (url, connections) = interrupting_registry(Interruption::Stall).await;
        let response = RequestBuilder::new(&impatient_client(), Method::GET, url.parse().unwrap())
            .send_and_read()
            .await
            .unwrap();

        assert_eq!(response.body, BODY);
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn status_errors_read_as_those_of_reqwest() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::NOT_FOUND,
            StatusCode::GONE,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let url = serve_connections(move |mut stream| async move {
                let _ = stream.read(&mut [0; 1024]).await;
                let response = format!(
                    "HTTP/1.1 {} {}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    status.as_str(),
                    status.canonical_reason().unwrap()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            })
            .await;

            let response = impatient_client().get(&url).send().await.unwrap();
            let url = response.url().clone();
            let expected = response.error_for_status().unwrap_err().to_string();

            let actual = FetchedResponse {
                status,
                headers: HeaderMap::new(),
                url,
                body: Bytes::new(),
            }
            .validate()
            .err()
            .unwrap()
            .to_string();

            assert_eq!(actual, expected);
        }

        let url: Url = "http://registry.test/artifact".parse().unwrap();
        let message = |status| check_status(status, &url).unwrap_err().to_string();

        assert_eq!(
            message(StatusCode::FOUND),
            "remote server attempted to redirect request - is this registry URL valid?"
        );
        assert_eq!(
            message(StatusCode::UNAUTHORIZED),
            "unauthorized - please provide registry credentials with `buffrs login`"
        );
        assert!(check_status(StatusCode::OK, &url).is_ok());
    }
}
