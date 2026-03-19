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

use miette::{miette, Context, IntoDiagnostic};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt::{self, Display},
    path::{Path, PathBuf},
    str::FromStr,
};
use tokio::fs;

use crate::{
    config::{self},
    errors::{DeserializationError, FileExistsError, SerializationError, WriteError},
    package::{PackageName, PackageType},
    registry::RegistryRef,
    ManagedFile,
};

/// The name of the manifest file
pub const MANIFEST_FILE: &str = "Proto.toml";

/// The canary edition supported by this version of buffrs
/// Note: This is independent of the crate version - only bump when the Proto.toml format changes
pub const CANARY_EDITION: &str = "0.50";

/// Edition of the buffrs manifest
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(into = "&str", from = "&str")]
pub enum Edition {
    /// The canary edition of manifests (0.50)
    ///
    /// This edition introduces multi-version dependency support.
    Canary,
    /// The canary edition used by buffrs 0.10.x
    Canary10,
    /// The canary edition used by buffrs 0.9.x
    Canary09,
    /// The canary edition used by buffrs 0.8.x
    Canary08,
    /// The canary edition used by buffrs 0.7.x
    Canary07,
    /// Unknown edition of manifests
    ///
    /// This is unrecommended as breaking changes could be introduced due to being
    /// in the beta release channel
    Unknown,
}

impl Edition {
    /// The current / latest edition of buffrs
    #[must_use]
    pub fn latest() -> Self {
        Self::Canary
    }
}

impl From<&str> for Edition {
    fn from(value: &str) -> Self {
        match value {
            // CANARY_EDITION is "0.50" - the current proto.toml format version with multi-version support
            self::CANARY_EDITION => Self::Canary,
            "0.10" => Self::Canary10,
            "0.9" => Self::Canary09,
            "0.8" => Self::Canary08,
            "0.7" => Self::Canary07,
            _ => Self::Unknown,
        }
    }
}

impl From<Edition> for &'static str {
    fn from(value: Edition) -> Self {
        match value {
            Edition::Canary => CANARY_EDITION,
            Edition::Canary10 => "0.10",
            Edition::Canary09 => "0.9",
            Edition::Canary08 => "0.8",
            Edition::Canary07 => "0.7",
            Edition::Unknown => "unknown",
        }
    }
}

/// A buffrs manifest format used for serialization and deserialization.
///
/// This contains the exact structure of the `Proto.toml` and skips
/// empty fields.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RawManifest {
    Canary {
        edition: Edition,
        package: Option<PackageManifest>,
        dependencies: DependencyMap,
    },
    Unknown {
        package: Option<PackageManifest>,
        dependencies: DependencyMap,
    },
}

impl RawManifest {
    fn package(&self) -> Option<&PackageManifest> {
        match self {
            Self::Canary { package, .. } => package.as_ref(),
            Self::Unknown { package, .. } => package.as_ref(),
        }
    }

    fn dependencies(&self) -> &DependencyMap {
        match self {
            Self::Canary { dependencies, .. } => dependencies,
            Self::Unknown { dependencies, .. } => dependencies,
        }
    }

    fn edition(&self) -> Edition {
        match self {
            Self::Canary { edition, .. } => edition.clone(),
            Self::Unknown { .. } => Edition::Unknown,
        }
    }
}

mod serializer {
    use super::*;
    use serde::{ser::SerializeStruct, Serializer};

    impl Serialize for RawManifest {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            match *self {
                RawManifest::Canary {
                    ref edition,
                    ref package,
                    ref dependencies,
                } => {
                    let edition_str: &str = edition.clone().into();
                    let mut s = serializer.serialize_struct("Canary", 3)?;
                    s.serialize_field("edition", edition_str)?;
                    s.serialize_field("package", package)?;
                    s.serialize_field("dependencies", dependencies)?;
                    s.end()
                }
                RawManifest::Unknown {
                    ref package,
                    ref dependencies,
                } => {
                    let mut s = serializer.serialize_struct("Unknown", 2)?;
                    s.serialize_field("package", package)?;
                    s.serialize_field("dependencies", dependencies)?;
                    s.end()
                }
            }
        }
    }
}

mod deserializer {
    use serde::{
        de::{self, MapAccess, Visitor},
        Deserializer,
    };

    use super::*;

    impl<'de> Deserialize<'de> for RawManifest {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            static FIELDS: &[&str] = &["package", "dependencies"];

            struct ManifestVisitor;

            impl<'de> Visitor<'de> for ManifestVisitor {
                type Value = RawManifest;

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str("a buffrs manifest (`Proto.toml`)")
                }

                fn visit_map<V>(self, mut map: V) -> Result<RawManifest, V::Error>
                where
                    V: MapAccess<'de>,
                {
                    let mut edition: Option<String> = None;
                    let mut package: Option<PackageManifest> = None;
                    let mut dependencies: Option<HashMap<PackageName, DependencyManifest>> = None;

                    while let Some(key) = map.next_key::<String>()? {
                        match key.as_str() {
                            "package" => package = Some(map.next_value()?),
                            "dependencies" => {
                                dependencies = Some(map.next_value()?);
                            }
                            "edition" => edition = Some(map.next_value()?),
                            _ => return Err(de::Error::unknown_field(&key, FIELDS)),
                        }
                    }

                    let dependencies = dependencies.unwrap_or_default();

                    let Some(edition) = edition else {
                        return Ok(RawManifest::Unknown {
                            package,
                            dependencies,
                        });
                    };

                    let parsed_edition = Edition::from(edition.as_str());
                    match parsed_edition {
                        Edition::Canary | Edition::Canary10 | Edition::Canary09 | Edition::Canary08 | Edition::Canary07 => Ok(RawManifest::Canary {
                            edition: parsed_edition,
                            package,
                            dependencies,
                        }),
                        Edition::Unknown => Err(de::Error::custom(
                            format!("unsupported manifest edition '{}', supported editions of buffrs {} are: {CANARY_EDITION}, 0.10, 0.9, 0.8, 0.7", edition, env!("CARGO_PKG_VERSION"))
                        )),
                    }
                }
            }

            deserializer.deserialize_map(ManifestVisitor)
        }
    }
}

impl From<Manifest> for RawManifest {
    fn from(manifest: Manifest) -> Self {
        let dependencies: DependencyMap = manifest
            .dependencies
            .into_iter()
            .map(|dep| (dep.package, dep.manifest))
            .collect();

        match manifest.edition {
            Edition::Canary
            | Edition::Canary10
            | Edition::Canary09
            | Edition::Canary08
            | Edition::Canary07 => RawManifest::Canary {
                edition: manifest.edition,
                package: manifest.package,
                dependencies,
            },
            Edition::Unknown => RawManifest::Unknown {
                package: manifest.package,
                dependencies,
            },
        }
    }
}

impl FromStr for RawManifest {
    type Err = toml::de::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        toml::from_str(input)
    }
}

/// Map representation of the dependency list
pub type DependencyMap = HashMap<PackageName, DependencyManifest>;

/// The buffrs manifest format used for internal processing, contains a parsed
/// version of the `RawManifest` for easier use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Edition of this manifest
    pub edition: Edition,
    /// Metadata about the root package
    pub package: Option<PackageManifest>,
    /// List of packages the root package depends on
    pub dependencies: Vec<Dependency>,
}

/// A resolved version of the manifest with registry aliases and local dependencies resolved
pub struct ResolvedManifest(pub Manifest);

impl Manifest {
    /// Create a new manifest of the current edition
    #[must_use]
    pub fn new(package: Option<PackageManifest>, dependencies: Vec<Dependency>) -> Self {
        Self {
            edition: Edition::latest(),
            package,
            dependencies,
        }
    }

    /// Checks if the manifest file exists in the filesystem
    pub async fn exists() -> miette::Result<bool> {
        fs::try_exists(MANIFEST_FILE)
            .await
            .into_diagnostic()
            .wrap_err(FileExistsError(MANIFEST_FILE.to_string()))
    }

    /// Loads the manifest from the current directory
    pub async fn read(config: &config::Config) -> miette::Result<Self> {
        Self::try_read_from(MANIFEST_FILE, Some(config))
            .await?
            .ok_or(miette!("`{MANIFEST_FILE}` does not exist"))
    }

    /// Parses the manifest from the given string
    ///
    /// # Arguments
    /// * `contents` - The contents of the manifest file
    /// * `config` - The configuration to use for resolving registry aliases
    pub fn try_parse(contents: &str, config: Option<&config::Config>) -> miette::Result<Self> {
        let raw: RawManifest = toml::from_str(contents)
            .into_diagnostic()
            .wrap_err(DeserializationError(ManagedFile::Manifest))?;

        let dependencies = raw
            .dependencies()
            .iter()
            .map(|(toml_key, manifest)| {
                let (package, resolved_manifest) = match manifest {
                    DependencyManifest::Remote(remote_manifest) => {
                        // Use explicit package name if provided, otherwise use the TOML key
                        let package = remote_manifest
                            .package
                            .clone()
                            .unwrap_or_else(|| toml_key.clone());
                        // For remote manifest dependencies, resolve the registry alias
                        let resolved_manifest =
                            DependencyManifest::Remote(RemoteDependencyManifest {
                                package: remote_manifest.package.clone(),
                                version: remote_manifest.version.clone(),
                                repository: remote_manifest.repository.clone(),
                                registry: remote_manifest.registry.with_alias_resolved(config)?,
                                resolver: remote_manifest.resolver,
                                namespace_overlap: remote_manifest.namespace_overlap,
                            });
                        (package, resolved_manifest)
                    }
                    DependencyManifest::Local(local_manifest) => {
                        // For local dependencies, check if a remote manifest is present
                        // and resolve its registry alias
                        // Use explicit package name if provided, otherwise use the TOML key
                        let package = local_manifest
                            .package
                            .clone()
                            .or_else(|| {
                                local_manifest
                                    .publish
                                    .as_ref()
                                    .and_then(|p| p.package.clone())
                            })
                            .unwrap_or_else(|| toml_key.clone());
                        if let Some(ref remote_manifest) = local_manifest.publish {
                            let resolved_manifest =
                                DependencyManifest::Local(LocalDependencyManifest {
                                    package: local_manifest.package.clone(),
                                    path: local_manifest.path.clone(),
                                    publish: Some(RemoteDependencyManifest {
                                        package: remote_manifest.package.clone(),
                                        version: remote_manifest.version.clone(),
                                        repository: remote_manifest.repository.clone(),
                                        registry: remote_manifest
                                            .registry
                                            .with_alias_resolved(config)?,
                                        resolver: remote_manifest.resolver,
                                        namespace_overlap: remote_manifest.namespace_overlap,
                                    }),
                                });
                            (package, resolved_manifest)
                        } else {
                            (package, manifest.clone())
                        }
                    }
                };

                Ok(Dependency {
                    package,
                    manifest: resolved_manifest,
                })
            })
            .collect::<miette::Result<Vec<_>>>()?;

        Ok(Self {
            edition: raw.edition(),
            package: raw.package().cloned(),
            dependencies,
        })
    }

    /// Loads the manifest from the given path
    ///
    /// # Arguments
    /// * `path` - The path to the manifest file
    /// * `config` - The configuration to use for resolving registry aliases
    pub async fn try_read_from(
        path: impl AsRef<Path>,
        config: Option<&config::Config>,
    ) -> miette::Result<Option<Self>> {
        let contents = match fs::read_to_string(path.as_ref()).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => {
                return Err(e).into_diagnostic().wrap_err(miette!(
                    "failed to read manifest from `{}`",
                    path.as_ref().display()
                ));
            }
        };

        Ok(Some(Self::try_parse(&contents, config)?))
    }

    /// Persists the manifest into the current directory
    pub async fn write(&self) -> miette::Result<()> {
        self.write_at(Path::new(".")).await
    }

    /// Persists the manifest into the provided directory, which must exist
    pub async fn write_at(&self, dir_path: &Path) -> miette::Result<()> {
        // hint: create a canary manifest from the current one
        let raw = RawManifest::from(Manifest::new(
            self.package.clone(),
            self.dependencies.clone(),
        ));

        let manifest_file_path = dir_path.join(MANIFEST_FILE);
        fs::write(
            manifest_file_path,
            toml::to_string(&raw)
                .into_diagnostic()
                .wrap_err(SerializationError(ManagedFile::Manifest))?
                .into_bytes(),
        )
        .await
        .into_diagnostic()
        .wrap_err(WriteError(MANIFEST_FILE))
    }

    /// Tests if the manifest is fully resolved (only contains remote dependencies)
    pub fn assert_fully_resolved(&self) -> miette::Result<()> {
        for dependency in &self.dependencies {
            if dependency.manifest.is_local() {
                return Err(miette!(
                    "dependency {} of {} does not specify version/registry/repository",
                    dependency.package,
                    self.package
                        .as_ref()
                        .map(|p| p.name.clone())
                        .map_or("package".to_string(), |n| n.to_string())
                ));
            }
        }

        Ok(())
    }
}

impl ResolvedManifest {
    /// Returns a clone of this manifest suitable for publishing
    ///
    /// - All local manifest dependencies are replaced with their remote counterparts
    pub fn new_from_manifest(mut manifest: Manifest) -> miette::Result<Self> {
        // Resolve aliases in dependencies prior to packaging
        for dependency in &mut manifest.dependencies {
            if let DependencyManifest::Local(ref local_manifest) = dependency.manifest {
                match local_manifest.publish {
                    Some(ref remote_manifest) => {
                        dependency.manifest = DependencyManifest::Remote(remote_manifest.clone());
                    }
                    None => {
                        return Err(miette!(
                            "local dependency {} of {} does not specify version/registry/repository",
                            dependency.package,
                            manifest.package
                                .as_ref()
                                .map(|p| p.name.clone())
                                .map_or("package".to_string(), |n| n.to_string())
                        ));
                    }
                }
            }
        }

        Ok(Self(manifest))
    }
}

/// Trait for serializable manifest types
pub trait PublishableManifest {
    /// Returns the header for the manifest file
    fn header() -> Option<&'static str>;
    /// Returns the name of the manifest file
    fn file_name() -> String;
}

impl PublishableManifest for Manifest {
    fn header() -> Option<&'static str> {
        None
    }

    fn file_name() -> String {
        format!("{MANIFEST_FILE}.orig")
    }
}

impl PublishableManifest for ResolvedManifest {
    fn header() -> Option<&'static str> {
        const MANIFEST_PREFIX: &str = r#"# THIS FILE IS AUTOMATICALLY GENERATED BY BUFFRS
#
# When uploading packages to the registry buffrs will automatically
# "normalize" Proto.toml files for maximal compatibility
# with all versions of buffrs and also rewrite `path` dependencies
# to registry dependencies.
#
# If you are reading this file be aware that the original Proto.toml
# will likely look very different (and much more reasonable).
# See Proto.toml.orig for the original contents.
"#;

        Some(MANIFEST_PREFIX)
    }

    fn file_name() -> String {
        MANIFEST_FILE.to_owned()
    }
}

impl TryInto<ResolvedManifest> for Manifest {
    type Error = miette::Report;

    fn try_into(self) -> Result<ResolvedManifest, Self::Error> {
        ResolvedManifest::new_from_manifest(self)
    }
}

impl TryInto<String> for Manifest {
    type Error = toml::ser::Error;

    fn try_into(self) -> Result<String, Self::Error> {
        toml::to_string_pretty(&RawManifest::from(self))
    }
}

impl TryInto<String> for ResolvedManifest {
    type Error = toml::ser::Error;

    fn try_into(self) -> Result<String, Self::Error> {
        self.0.try_into()
    }
}

/// Manifest format for api packages
#[derive(Debug, Clone, Hash, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageManifest {
    /// Type of the package
    #[serde(rename = "type")]
    pub kind: PackageType,
    /// Name of the package
    pub name: PackageName,
    /// Version of the package
    pub version: Version,
    /// Description of the api package
    pub description: Option<String>,
}

/// Represents a single project dependency
#[derive(Clone, Debug, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dependency {
    /// Package name of this dependency
    pub package: PackageName,
    /// Version requirement in the buffrs format, currently only supports pinning
    pub manifest: DependencyManifest,
}

impl Dependency {
    /// Creates a new dependency
    #[must_use]
    pub fn new(
        registry: &RegistryRef,
        repository: String,
        package: PackageName,
        version: VersionReq,
    ) -> Self {
        Self {
            package: package.clone(),
            manifest: RemoteDependencyManifest {
                // No aliasing - key name matches package name
                package: None,
                repository,
                version,
                registry: registry.to_owned(),
                resolver: ResolverMode::default(),
                namespace_overlap: NamespaceOverlapPolicy::default(),
            }
            .into(),
        }
    }

    /// Creates a copy of this dependency with a pinned version
    #[must_use]
    pub fn with_version(&self, version: &Version) -> Dependency {
        let mut dependency = self.clone();

        if let DependencyManifest::Remote(ref mut manifest) = dependency.manifest {
            manifest.version = VersionReq {
                comparators: vec![semver::Comparator {
                    op: semver::Op::Exact,
                    major: version.major,
                    minor: Some(version.minor),
                    patch: Some(version.patch),
                    pre: version.pre.clone(),
                }],
            };
        }

        dependency
    }

    /// Returns the resolver mode for this dependency edge.
    #[must_use]
    pub fn resolver_mode(&self) -> ResolverMode {
        match &self.manifest {
            DependencyManifest::Remote(m) => m.resolver,
            DependencyManifest::Local(m) => {
                m.publish.as_ref().map(|p| p.resolver).unwrap_or_default()
            }
        }
    }

    /// Returns the namespace overlap policy for this dependency edge.
    #[must_use]
    pub fn namespace_overlap_policy(&self) -> NamespaceOverlapPolicy {
        match &self.manifest {
            DependencyManifest::Remote(m) => m.namespace_overlap,
            DependencyManifest::Local(m) => m
                .publish
                .as_ref()
                .map(|p| p.namespace_overlap)
                .unwrap_or_default(),
        }
    }

    /// Returns true if this dependency allows multiple versions.
    #[must_use]
    pub fn allows_multiversion(&self) -> bool {
        matches!(self.resolver_mode(), ResolverMode::MultiVersion)
    }
}

impl Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.manifest {
            DependencyManifest::Remote(manifest) => write!(
                f,
                "{}/{}@{}",
                manifest.repository, self.package, manifest.version
            ),
            DependencyManifest::Local(manifest) => {
                write!(f, "{}@{}", self.package, manifest.path.display())
            }
        }
    }
}

/// Manifest format for dependencies
#[derive(Debug, Clone, Hash, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum DependencyManifest {
    /// A remote dependency from artifactory
    Remote(RemoteDependencyManifest),
    /// A local dependency located on the filesystem
    Local(LocalDependencyManifest),
}

impl DependencyManifest {
    pub(crate) fn is_local(&self) -> bool {
        matches!(self, DependencyManifest::Local(_))
    }
}

/// Resolver mode for a dependency edge.
///
/// Controls whether multiple versions of the same package name can coexist.
#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResolverMode {
    /// Default: single-version resolution (one version per package name)
    #[default]
    Default,
    /// Allow multiple versions of this package if constraints require it
    #[serde(rename = "multiversion")]
    MultiVersion,
}

/// Namespace overlap policy for multi-version scenarios.
///
/// Controls what happens when multiple versions declare the same protobuf namespace.
#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum NamespaceOverlapPolicy {
    /// Default: rewrite proto package declarations to include version suffix
    /// This ensures safe multi-version coexistence by generating unique namespaces.
    #[default]
    Rewrite,
    /// Allow overlap only if proto file content hashes match (no rewriting needed)
    IdenticalOnly,
    /// Fail if two versions declare the same namespace (legacy behavior)
    Forbidden,
}

/// Manifest format for dependencies
#[derive(Debug, Clone, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteDependencyManifest {
    /// Actual package name (if different from TOML key name, for aliasing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageName>,
    /// Version requirement in the buffrs format, currently only supports pinning
    pub version: VersionReq,
    /// Artifactory repository to pull dependency from
    pub repository: String,
    /// Artifactory registry to pull from
    pub registry: RegistryRef,
    /// Resolver mode for this dependency edge (default: single-version)
    #[serde(default, skip_serializing_if = "is_default_resolver")]
    pub resolver: ResolverMode,
    /// Namespace overlap policy when multi-version is enabled
    #[serde(default, skip_serializing_if = "is_default_namespace_policy")]
    pub namespace_overlap: NamespaceOverlapPolicy,
}

fn is_default_resolver(mode: &ResolverMode) -> bool {
    matches!(mode, ResolverMode::Default)
}

fn is_default_namespace_policy(policy: &NamespaceOverlapPolicy) -> bool {
    matches!(policy, NamespaceOverlapPolicy::Rewrite)
}

impl From<RemoteDependencyManifest> for DependencyManifest {
    fn from(value: RemoteDependencyManifest) -> Self {
        Self::Remote(value)
    }
}

/// Manifest format for local filesystem dependencies
#[derive(Debug, Clone, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalDependencyManifest {
    /// Actual package name (if different from TOML key name, for aliasing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageName>,
    /// Path to local buffrs package
    pub path: PathBuf,
    /// Optional remote manifest for publishing
    #[serde(flatten)]
    pub publish: Option<RemoteDependencyManifest>,
}

impl From<LocalDependencyManifest> for DependencyManifest {
    fn from(value: LocalDependencyManifest) -> Self {
        Self::Local(value)
    }
}

// Custom deserialization logic for `DependencyManifest`
mod dependency_manifest_deserializer {
    use super::*;
    use serde::{de::Error, Deserialize, Deserializer};

    impl<'de> Deserialize<'de> for DependencyManifest {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            #[derive(Deserialize)]
            struct TempManifest {
                /// Actual package name (for aliasing: key is alias, this is real name)
                package: Option<PackageName>,
                path: Option<PathBuf>,
                version: Option<VersionReq>,
                repository: Option<String>,
                registry: Option<RegistryRef>,
                #[serde(default)]
                resolver: ResolverMode,
                #[serde(default)]
                namespace_overlap: NamespaceOverlapPolicy,
            }

            let temp: TempManifest = TempManifest::deserialize(deserializer)?;

            if let Some(path) = temp.path {
                // Deserialize as a local dependency with optional remote attributes
                Ok(DependencyManifest::Local(LocalDependencyManifest {
                    package: temp.package.clone(),
                    path,
                    publish: match (temp.version, temp.repository) {
                        (Some(version), Some(repository)) => {
                            // Use explicit registry or UseDefault if not specified
                            let registry = temp
                                .registry
                                .unwrap_or(crate::registry::RegistryRef::UseDefault);
                            Some(RemoteDependencyManifest {
                                package: temp.package.clone(),
                                version,
                                repository,
                                registry,
                                resolver: temp.resolver,
                                namespace_overlap: temp.namespace_overlap,
                            })
                        }
                        _ => None,
                    },
                }))
            } else if let (Some(version), Some(repository)) = (temp.version, temp.repository) {
                // Deserialize as a remote dependency
                // Use explicit registry or UseDefault if not specified
                let registry = temp
                    .registry
                    .unwrap_or(crate::registry::RegistryRef::UseDefault);
                Ok(DependencyManifest::Remote(RemoteDependencyManifest {
                    package: temp.package,
                    version,
                    repository,
                    registry,
                    resolver: temp.resolver,
                    namespace_overlap: temp.namespace_overlap,
                }))
            } else {
                Err(D::Error::custom("Invalid dependency manifest"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test: Proto.toml without [dependencies] should parse as empty dependencies.
    /// See issue #23.
    #[test]
    fn manifest_without_dependencies_parses_as_empty() {
        let toml = r#"
edition = "0.50"

[package]
name = "test-package"
version = "1.0.0"
type = "lib"
"#;
        let manifest = Manifest::try_parse(toml, None).expect("should parse successfully");
        assert!(
            manifest.dependencies.is_empty(),
            "expected zero dependencies, got {:?}",
            manifest.dependencies
        );
        assert!(manifest.package.is_some());
        assert_eq!(
            manifest.package.as_ref().unwrap().name.to_string(),
            "test-package"
        );
    }

    /// Verify that an empty dependencies table also works.
    #[test]
    fn manifest_with_empty_dependencies_table() {
        let toml = r#"
edition = "0.50"

[package]
name = "another-package"
version = "2.0.0"
type = "api"

[dependencies]
"#;
        let manifest = Manifest::try_parse(toml, None).expect("should parse successfully");
        assert!(manifest.dependencies.is_empty());
    }

    /// Verify that omitting registry uses the default registry.
    #[test]
    fn dependency_without_registry_uses_default() {
        use crate::registry::RegistryRef;

        let toml = r#"
edition = "0.50"

[package]
name = "test-package"
version = "1.0.0"
type = "lib"

[dependencies]
lib-algo-base = { version = "=0.1.3-SPINE-4384", repository = "grpc" }
"#;
        // Parse should succeed without config (registry will be UseDefault)
        let manifest = Manifest::try_parse(toml, None).expect("should parse successfully");
        assert_eq!(manifest.dependencies.len(), 1);

        let dep = &manifest.dependencies[0];
        assert_eq!(dep.package.to_string(), "lib-algo-base");

        // Verify the registry is UseDefault
        match &dep.manifest {
            DependencyManifest::Remote(remote) => {
                assert!(
                    matches!(remote.registry, RegistryRef::UseDefault),
                    "expected UseDefault registry, got {:?}",
                    remote.registry
                );
            }
            other => panic!("expected Remote dependency, got {:?}", other),
        }
    }

    /// Regression test: edition must survive a parse → serialize round-trip.
    ///
    /// Before the fix, `RawManifest::Canary` did not store the parsed edition
    /// and the serializer hardcoded `CANARY_EDITION` ("0.50"), so any manifest
    /// with `edition = "0.10"` would be rewritten to `edition = "0.50"`.
    #[test]
    fn edition_preserved_on_roundtrip() {
        for edition in &["0.10", "0.9", "0.8", "0.7", "0.50"] {
            let input = format!(
                r#"edition = "{edition}"

[package]
type = "lib"
name = "test-pkg"
version = "1.0.0"
"#
            );
            let manifest = Manifest::try_parse(&input, None)
                .unwrap_or_else(|e| panic!("should parse edition {edition}: {e}"));
            let output: String = manifest
                .try_into()
                .unwrap_or_else(|e| panic!("should serialize edition {edition}: {e}"));
            assert!(
                output.contains(&format!(r#"edition = "{edition}""#)),
                "Edition {edition} was mutated during round-trip! Output:\n{output}"
            );
        }
    }
}
