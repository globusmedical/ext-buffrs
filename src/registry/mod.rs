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

use std::{
    fmt::{self, Display},
    str::FromStr,
};

mod artifactory;
#[cfg(test)]
mod cache;

use crate::manifest::DependencyManifest;
use crate::{config, manifest::Dependency};
pub use artifactory::{build_reqwest_client, Artifactory, CertValidationPolicy, ENV_CA_BUNDLE};
use miette::{ensure, miette, Context, IntoDiagnostic};
use semver::VersionReq;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// A representation of a registry URI
#[derive(Debug, Clone, Hash, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegistryUri(Url);

impl RegistryUri {
    /// Get the host component of the registry URI
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        self.0.host_str()
    }

    /// Get the path component of the registry URI
    #[must_use]
    pub fn path(&self) -> &str {
        self.0.path()
    }
}

/// A reference to a registry
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegistryRef {
    /// A URL to a registry
    Url(RegistryUri),
    /// An alias to a registry
    Alias(String),
    /// A resolved alias to a registry
    ResolvedAlias {
        /// The alias
        alias: String,
        /// The resolved URL
        url: RegistryUri,
    },
    /// Placeholder: use the default registry from config.
    /// This is set during manifest parsing when the `registry` field is omitted.
    /// It must be resolved before use by calling `with_default_resolved`.
    UseDefault,
}

impl RegistryRef {
    /// Get the raw URL of the registry with any alias or default resolved
    ///
    /// # Arguments
    /// * `config` - The configuration to use to resolve the alias or default
    ///
    /// If config is `None` and the registry is `UseDefault`, this returns
    /// `UseDefault` unchanged (deferred resolution). Actual installation
    /// commands will have config available to resolve it.
    pub fn with_alias_resolved(&self, config: Option<&config::Config>) -> miette::Result<Self> {
        match self {
            RegistryRef::Alias(alias) => match config {
                Some(config) => {
                    let url = config.lookup_registry(alias)?;
                    Ok(RegistryRef::ResolvedAlias {
                        alias: alias.clone(),
                        url: url.clone(),
                    })
                }
                None => Err(miette!(
                    "no configuration provided to resolve alias \"{}\"",
                    alias
                )),
            },
            RegistryRef::UseDefault => match config {
                Some(config) => {
                    // Get the default registry from config and resolve it
                    let default_ref = config.parse_registry_arg(&None)?;
                    // Recursively resolve in case the default is itself an alias
                    default_ref.with_alias_resolved(Some(config))
                }
                // When no config is available, preserve UseDefault for later resolution
                None => Ok(self.clone()),
            },
            _ => Ok(self.clone()),
        }
    }

    /// Serializer for resolved `RegistryUris`
    pub fn serialize_resolved<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RegistryRef::ResolvedAlias { url, .. } => url.serialize(serializer),
            RegistryRef::Url(url) => url.serialize(serializer),
            _ => Err(serde::ser::Error::custom(
                "cannot serialize unresolved alias",
            )),
        }
    }
}

impl<'de> Deserialize<'de> for RegistryRef {
    fn deserialize<D>(deserializer: D) -> Result<RegistryRef, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let url = String::deserialize(deserializer)?;
        RegistryRef::from_str(&url).map_err(serde::de::Error::custom)
    }
}

impl Serialize for RegistryRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RegistryRef::ResolvedAlias { url, .. } => url.serialize(serializer),
            RegistryRef::Url(url) => url.serialize(serializer),
            RegistryRef::Alias(alias) => alias.serialize(serializer),
            RegistryRef::UseDefault => Err(serde::ser::Error::custom(
                "cannot serialize UseDefault registry reference; it must be resolved first",
            )),
        }
    }
}

impl From<RegistryUri> for Url {
    fn from(value: RegistryUri) -> Self {
        value.0
    }
}

impl TryFrom<RegistryRef> for RegistryUri {
    type Error = miette::Report;

    fn try_from(value: RegistryRef) -> Result<Self, Self::Error> {
        match value {
            RegistryRef::Url(url) => Ok(url),
            RegistryRef::ResolvedAlias { url, .. } => Ok(url),
            _ => Err(miette!(
                "cannot convert unresolved alias \"{value}\" to URL"
            )),
        }
    }
}

impl TryFrom<&RegistryRef> for RegistryUri {
    type Error = miette::Report;

    fn try_from(value: &RegistryRef) -> Result<Self, Self::Error> {
        // Delegate to the implementation for the owned type
        TryFrom::<RegistryRef>::try_from(value.clone())
    }
}

impl Display for RegistryRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryRef::Url(url) => write!(f, "{url}"),
            RegistryRef::Alias(alias) => write!(f, "{alias}"),
            RegistryRef::ResolvedAlias { alias, url } => write!(f, "{alias} ({url})"),
            RegistryRef::UseDefault => write!(f, "<default>"),
        }
    }
}

impl FromStr for RegistryRef {
    type Err = miette::Report;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Attempt to parse the value as a URL
        match RegistryUri::from_str(value) {
            Ok(uri) => Ok(Self::Url(uri)),
            Err(_) => {
                // Handle legacy "alias (url)" format from older buffrs versions
                // (before v0.10.1) that incorrectly serialized ResolvedAlias
                // using its Display format instead of just the URL.
                if let Some(resolved) = parse_legacy_resolved_alias(value) {
                    tracing::warn!(
                        "recovered corrupted registry reference \"{value}\" \
                         (legacy \"alias (url)\" format)"
                    );
                    return Ok(resolved);
                }

                Ok(Self::Alias(value.to_owned()))
            }
        }
    }
}

impl Display for RegistryUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for RegistryUri {
    type Err = miette::Report;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let url = Url::from_str(value)
            .into_diagnostic()
            .wrap_err(miette!("not a valid URL: {value}"))?;

        sanity_check_url(&url)?;

        Ok(Self(url))
    }
}

/// Attempt to parse a legacy `"alias (url)"` string produced by buffrs
/// versions before v0.10.1, which incorrectly serialized `ResolvedAlias`
/// using its `Display` implementation instead of just the URL.
///
/// Returns `Some(ResolvedAlias { alias, url })` when the pattern matches
/// and the extracted URL is valid, `None` otherwise.
fn parse_legacy_resolved_alias(value: &str) -> Option<RegistryRef> {
    // Pattern: "alias_name (https://...)"
    let open = value.find(" (")?;
    let alias = &value[..open];

    // Alias must be non-empty and must not look like a URL scheme
    if alias.is_empty() || alias.contains("://") {
        return None;
    }

    // Strip the surrounding parentheses
    let url_part = value.get(open + 2..value.len().checked_sub(1)?)?;
    if !value.ends_with(')') {
        return None;
    }

    let url = RegistryUri::from_str(url_part).ok()?;
    Some(RegistryRef::ResolvedAlias {
        alias: alias.to_owned(),
        url,
    })
}

/// Ensure that the URL is valid for a registry
///
/// A valid registry URL must:
/// - Have a scheme of either "http" or "https"
/// - End with "/artifactory" if the host is a `JFrog` Artifactory instance
/// - Have a host component
///
/// # Arguments
/// * `url` - The URL to check
pub fn sanity_check_url(url: &Url) -> miette::Result<()> {
    let scheme = url.scheme();

    ensure!(
        scheme == "http" || scheme == "https",
        "invalid URI scheme {scheme} - must be http or https"
    );

    if let Some(host) = url.host_str() {
        ensure!(
            !host.ends_with(".jfrog.io") || url.path().ends_with("/artifactory"),
            "the url must end with '/artifactory' when using a *.jfrog.io host"
        );
        Ok(())
    } else {
        Err(miette!("the URI must contain a host component: {url}"))
    }
}

#[derive(Error, Debug)]
#[error("{0} is not a supported version requirement")]
struct UnsupportedVersionRequirement(VersionReq);

#[derive(Error, Debug)]
#[error("{0} is not supported yet. Pin the exact version you want to use with '='. For example: '=1.0.4' instead of '^1.0.0'")]
struct VersionNotPinned(VersionReq);

fn dependency_version_string(dependency: &Dependency) -> miette::Result<String> {
    let DependencyManifest::Remote(ref manifest) = dependency.manifest else {
        return Err(miette!(
            "unable to serialize version of local dependency ({})",
            dependency.package
        ));
    };

    let version = manifest
        .version
        .comparators
        .first()
        .ok_or_else(|| UnsupportedVersionRequirement(manifest.version.clone()))
        .into_diagnostic()?;

    ensure!(
        version.op == semver::Op::Exact,
        VersionNotPinned(manifest.version.clone())
    );

    let minor_version = version
        .minor
        .ok_or_else(|| miette!("version missing minor number"))?;

    let patch_version = version
        .patch
        .ok_or_else(|| miette!("version missing patch number"))?;

    Ok(format!(
        "{}.{}.{}{}",
        version.major,
        minor_version,
        patch_version,
        if version.pre.is_empty() {
            String::new()
        } else {
            format!("-{}", version.pre)
        }
    ))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use semver::VersionReq;

    use crate::{
        manifest::Dependency,
        package::PackageName,
        registry::{dependency_version_string, VersionNotPinned},
    };

    use super::RegistryRef;

    fn get_dependency(version: &str) -> Dependency {
        let registry = RegistryRef::from_str("https://my-registry.com").unwrap();
        let repository = String::from("my-repo");
        let package = PackageName::from_str("package").unwrap();
        let version = VersionReq::from_str(version).unwrap();
        Dependency::new(&registry, repository, package, version)
    }

    #[test]
    fn valid_version() {
        let dependency = get_dependency("=0.0.1");
        assert!(dependency_version_string(&dependency).is_ok_and(|version| version == "0.0.1"));

        let dependency = get_dependency("=0.0.1-23");
        assert!(dependency_version_string(&dependency).is_ok_and(|version| version == "0.0.1-23"));

        let dependency = get_dependency("=0.0.1-ab");
        assert!(dependency_version_string(&dependency).is_ok_and(|version| version == "0.0.1-ab"));
    }

    #[test]
    fn unsupported_version_operator() {
        let dependency = get_dependency("^0.0.1");
        assert!(
            dependency_version_string(&dependency).is_err_and(|err| err.is::<VersionNotPinned>())
        );

        let dependency = get_dependency("~0.0.1");
        assert!(
            dependency_version_string(&dependency).is_err_and(|err| err.is::<VersionNotPinned>())
        );

        let dependency = get_dependency("<=0.0.1");
        assert!(
            dependency_version_string(&dependency).is_err_and(|err| err.is::<VersionNotPinned>())
        );
    }

    #[test]
    fn incomplete_version() {
        let dependency = get_dependency("=1.0");
        assert!(dependency_version_string(&dependency).is_err());

        let dependency = get_dependency("=1");
        assert!(dependency_version_string(&dependency).is_err());
    }

    #[test]
    fn from_str_url() {
        let reg = RegistryRef::from_str("https://conan-us.globusmedical.com/artifactory").unwrap();
        assert!(matches!(reg, RegistryRef::Url(_)));
    }

    #[test]
    fn from_str_alias() {
        let reg = RegistryRef::from_str("globus").unwrap();
        assert!(matches!(reg, RegistryRef::Alias(ref a) if a == "globus"));
    }

    #[test]
    fn from_str_legacy_resolved_alias() {
        // Older buffrs versions (< 0.10.1) serialized ResolvedAlias as
        // "alias (url)" via Display. Verify we recover gracefully.
        let input = "globus (https://conan-us.globusmedical.com/artifactory)";
        let reg = RegistryRef::from_str(input).unwrap();
        match reg {
            RegistryRef::ResolvedAlias { ref alias, ref url } => {
                assert_eq!(alias, "globus");
                assert_eq!(
                    url.to_string(),
                    "https://conan-us.globusmedical.com/artifactory"
                );
            }
            other => panic!("expected ResolvedAlias, got {other:?}"),
        }
    }

    #[test]
    fn from_str_legacy_resolved_alias_not_triggered_for_plain_alias() {
        // A plain alias without parenthesized URL must remain Alias
        let reg = RegistryRef::from_str("my-registry").unwrap();
        assert!(matches!(reg, RegistryRef::Alias(ref a) if a == "my-registry"));
    }

    #[test]
    fn from_str_legacy_resolved_alias_bad_url_stays_alias() {
        // If the URL inside parens is invalid, fall back to Alias
        let reg = RegistryRef::from_str("name (not-a-url)").unwrap();
        assert!(matches!(reg, RegistryRef::Alias(_)));
    }
}
