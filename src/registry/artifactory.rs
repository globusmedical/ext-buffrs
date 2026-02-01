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

/// Builds a configured reqwest::Client for Artifactory operations.
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

    /// Creates a new instance with a pre-built reqwest::Client.
    ///
    /// Use this when you want to share a connection pool across multiple
    /// Artifactory clients within the same invocation.
    ///
    /// # Arguments
    /// * `registry` - The registry URI
    /// * `credentials` - The credentials to use for the registry
    /// * `client` - A pre-configured reqwest::Client (use `build_reqwest_client`)
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
            let mut uri: url::Url = self.registry.to_owned().into();
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
    /// Returns a list of all valid semver versions found for the package.
    pub async fn list_versions(
        &self,
        repository: String,
        name: PackageName,
    ) -> miette::Result<Vec<Version>> {
        // Retrieve all packages matching the given name
        let search_query_url: Url = {
            let mut uri: url::Url = self.registry.to_owned().into();
            uri.set_path("artifactory/api/search/artifact");
            uri.set_query(Some(&format!("name={name}&repos={repository}")));
            uri
        };

        let response = self
            .new_request(Method::GET, search_query_url)
            .send()
            .await?;
        let response: reqwest::Response = response.0;

        let headers = response.headers();
        let content_type = headers
            .get(&reqwest::header::CONTENT_TYPE)
            .ok_or_else(|| miette!("missing content-type header"))?;
        ensure!(
            content_type
                == reqwest::header::HeaderValue::from_static(
                    "application/vnd.org.jfrog.artifactory.search.ArtifactSearchResult+json"
                ),
            "server response has incorrect mime type: {content_type:?}"
        );

        let response_str = response.text().await.into_diagnostic().wrap_err(miette!(
            "unexpected error: unable to retrieve response payload"
        ))?;
        let parsed_response = serde_json::from_str::<ArtifactSearchResponse>(&response_str)
            .into_diagnostic()
            .wrap_err(miette!(
                "unexpected error: response could not be deserialized to ArtifactSearchResponse"
            ))?;

        tracing::debug!(
            "List of artifacts found matching the name: {:?}",
            parsed_response
        );

        // Extract all valid versions from the artifact URIs
        let versions: Vec<Version> = parsed_response
            .results
            .iter()
            .filter_map(|artifact_search_result| {
                let uri = artifact_search_result.to_owned().uri;
                let full_artifact_name = uri
                    .split('/')
                    .next_back()
                    .map(|name_tgz| name_tgz.trim_end_matches(".tgz"));
                let artifact_version = full_artifact_name.and_then(|artifact_name| {
                    // Extract version by removing the package name prefix
                    // Format: {name}-{version}.tgz
                    let name_str = name.to_string();
                    artifact_name
                        .strip_prefix(&name_str)
                        .and_then(|rest| rest.strip_prefix('-'))
                        .and_then(|version_str| Version::parse(version_str).ok())
                });

                // Double-check that the artifact name matches exactly
                let expected_artifact_name =
                    artifact_version.clone().map(|av| format!("{name}-{av}"));
                if full_artifact_name.is_some_and(|actual| {
                    expected_artifact_name.is_some_and(|expected| expected == actual)
                }) {
                    artifact_version
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
        use crate::version::version_to_artifact_string;

        let artifact_url = {
            let version_str = version_to_artifact_string(version);
            let url: RegistryUri = self.registry.clone();
            let mut url: url::Url = url.into();
            let path = url.path();

            url.set_path(&format!(
                "{}/{}/{}/{}-{}.tgz",
                path, repository, name, name, version_str
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
            "failed to download dependency {}@{}",
            name,
            version
        ))
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
        self.0.send().await.into_diagnostic()?.try_into()
    }

    /// Send request and return raw response without validation.
    /// Used for checking artifact existence where 404 is an expected outcome.
    async fn send_raw(self) -> miette::Result<reqwest::Response> {
        self.0.send().await.into_diagnostic()
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
struct ArtifactSearchResponse {
    results: Vec<ArtifactSearchResult>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
struct ArtifactSearchResult {
    uri: String,
}
