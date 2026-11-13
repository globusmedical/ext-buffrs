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
use miette::{ensure, miette, Context, IntoDiagnostic};
use reqwest::{Body, Method, Response};
use semver::Version;
use serde::Deserialize;
use std::time::Duration;
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
pub fn build_reqwest_client(policy: CertValidationPolicy) -> miette::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(policy == CertValidationPolicy::NoValidation);

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
        let mut request_builder = RequestBuilder::new(self.client.clone(), method, url);

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
            .send_raw()
            .await?;

        // A 404 means the package folder doesn't exist yet — return empty
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!("Package folder not found at {storage_url}, returning empty versions");
            return Ok(Vec::new());
        }

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(miette!(
                "unauthorized - please provide registry credentials with `buffrs login`"
            ));
        }

        if !response.status().is_success() {
            return Err(miette!(
                "unexpected status {} from storage API at {storage_url}",
                response.status()
            ));
        }

        let response_str = response.text().await.into_diagnostic().wrap_err(miette!(
            "unexpected error: unable to retrieve response payload"
        ))?;
        let parsed_response = serde_json::from_str::<StorageFolderInfo>(&response_str)
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
        use reqwest::StatusCode;

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
            .send_raw()
            .await
            .map_err(DownloadFailure::Other)?;
        let status = response.status();
        let response = match ValidatedResponse::try_from(response) {
            Ok(response) => response.0,
            Err(e) if status == StatusCode::NOT_FOUND || status == StatusCode::GONE => {
                return Err(DownloadFailure::NotFound(e))
            }
            Err(e) if is_throttled(status) => return Err(DownloadFailure::Throttled(e)),
            Err(e) => return Err(DownloadFailure::Other(e)),
        };

        let content_type = response
            .headers()
            .get(&reqwest::header::CONTENT_TYPE)
            .ok_or_else(|| DownloadFailure::Other(miette!("missing content-type header")))?;

        if content_type != reqwest::header::HeaderValue::from_static("application/x-gzip") {
            return Err(DownloadFailure::Other(miette!(
                "server response has incorrect mime type: {content_type:?}"
            )));
        }

        let data = response
            .bytes()
            .await
            .into_diagnostic()
            .map_err(DownloadFailure::Other)?;

        Package::try_from(data)
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

        let response = self.new_request(Method::GET, artifact_url).send().await?;
        let response: reqwest::Response = response.0;
        let headers = response.headers();
        let content_type = headers
            .get(&reqwest::header::CONTENT_TYPE)
            .ok_or_else(|| miette!("missing content-type header"))?;

        ensure!(
            content_type == reqwest::header::HeaderValue::from_static("application/x-gzip"),
            "server response has incorrect mime type: {content_type:?}"
        );

        let data = response.bytes().await.into_diagnostic()?;

        Package::try_from(data).wrap_err(miette!(
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

struct RequestBuilder(reqwest::RequestBuilder);

impl RequestBuilder {
    fn new(client: reqwest::Client, method: reqwest::Method, url: Url) -> Self {
        Self(client.request(method, url))
    }

    fn auth(mut self, token: String) -> Self {
        self.0 = self.0.bearer_auth(token);
        self
    }

    fn body(mut self, payload: impl Into<Body>) -> Self {
        self.0 = self.0.body(payload);
        self
    }

    async fn send(self) -> miette::Result<ValidatedResponse> {
        self.send_with_retry().await?.try_into()
    }

    /// Send request and return raw response without validation.
    /// Used for checking artifact existence where 404 is an expected outcome.
    async fn send_raw(self) -> miette::Result<reqwest::Response> {
        self.send_with_retry().await
    }

    /// Sends the request, waiting and resending while the registry throttles it.
    ///
    /// A registry behind a rate limiter answers 429 (or 503 while overloaded) to a burst of
    /// requests, which several concurrent installs from one address produce. The request is
    /// resent up to [`MAX_SEND_ATTEMPTS`] times in total and for at most
    /// [`MAX_TOTAL_RETRY_WAIT`]; after that the throttled response is returned as is. A request
    /// whose body cannot be replayed is sent once.
    async fn send_with_retry(self) -> miette::Result<reqwest::Response> {
        let mut attempt = 1;
        let mut waited = Duration::ZERO;

        loop {
            let Some(request) = self.0.try_clone() else {
                return self.0.send().await.into_diagnostic();
            };

            let response = request.send().await.into_diagnostic()?;

            if !is_throttled(response.status()) || attempt >= MAX_SEND_ATTEMPTS {
                return Ok(response);
            }

            let Some(delay) = retry_delay(response.headers(), attempt)
                .filter(|delay| waited + *delay <= MAX_TOTAL_RETRY_WAIT)
            else {
                return Ok(response);
            };

            tracing::warn!(
                "registry answered {} for {}, retrying in {:.1}s (attempt {attempt} of {MAX_SEND_ATTEMPTS})",
                response.status(),
                response.url(),
                delay.as_secs_f64()
            );

            tokio::time::sleep(delay).await;
            waited += delay;
            attempt += 1;
        }
    }
}

/// Total number of times a throttled request is sent before its response is returned as is.
const MAX_SEND_ATTEMPTS: u32 = 6;

/// Longest `Retry-After` the client waits for. A longer cooldown ends the retries.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Shortest wait before a resend, used when the registry sends `Retry-After: 0`.
const MIN_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Longest time one request spends waiting between its attempts.
const MAX_TOTAL_RETRY_WAIT: Duration = Duration::from_secs(120);

/// Whether the status asks the client to slow down and try again.
fn is_throttled(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
}

/// Time to wait before resending after the given attempt (1-based).
///
/// A `Retry-After` header given in seconds is never shortened, so the request is not resent
/// before the cooldown the registry asked for. `None` means that cooldown exceeds
/// [`MAX_RETRY_AFTER`] and the request should not be resent. Without the header, or with one
/// given as an HTTP date, the wait doubles from one second per attempt. Every wait gets up to
/// 50% jitter on top so that concurrent installs spread out.
fn retry_delay(headers: &reqwest::header::HeaderMap, attempt: u32) -> Option<Duration> {
    let retry_after = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs);

    let base = match retry_after {
        Some(retry_after) if retry_after > MAX_RETRY_AFTER => return None,
        Some(retry_after) => retry_after.max(MIN_RETRY_DELAY),
        None => Duration::from_secs(1) * 2u32.saturating_pow(attempt.saturating_sub(1)),
    };

    Some(base + base.mul_f64(0.5 * jitter_fraction()))
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

struct ValidatedResponse(reqwest::Response);

impl TryFrom<Response> for ValidatedResponse {
    type Error = miette::Report;

    fn try_from(value: Response) -> Result<Self, Self::Error> {
        ensure!(
            !value.status().is_redirection(),
            "remote server attempted to redirect request - is this registry URL valid?"
        );

        ensure!(
            value.status() != 401,
            "unauthorized - please provide registry credentials with `buffrs login`"
        );

        value.error_for_status().into_diagnostic().map(Self)
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

    #[test]
    fn backoff_doubles_without_a_usable_header() {
        let http_date = retry_after("Wed, 21 Oct 2026 07:28:00 GMT");

        for headers in [HeaderMap::new(), http_date] {
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
}
