use async_recursion::async_recursion;
use miette::{bail, ensure, Context, Diagnostic, IntoDiagnostic};
use semver::{Version, VersionReq};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fmt,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};
use thiserror::Error;

use crate::{
    cache::{Cache, Entry},
    config::Config,
    credentials::Credentials,
    lock::{FileRequirement, Lockfile},
    manifest::{
        Dependency, DependencyManifest, LocalDependencyManifest, Manifest, NamespaceOverlapPolicy,
        RemoteDependencyManifest, ResolverMode, MANIFEST_FILE,
    },
    package::{Package, PackageName, PackageStore},
    pubgrub_resolver::{BuffrsDependencyProvider, PackageDependency, PackageInfo},
    registry::{Artifactory, CertValidationPolicy, RegistryRef, RegistryUri},
    version::{extract_exact_version, is_exact_requirement, select_version},
};

/// Uniquely identifies a resolved package instance.
///
/// In the default mode buffrs resolves at most one version per package name. When
/// `Config::allow_multiple_versions()` is enabled, the resolver may keep multiple
/// resolved versions of the same package name side-by-side.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedPackageId {
    name: PackageName,
    version: Version,
}

impl ResolvedPackageId {
    /// Creates a new resolved package identifier.
    pub fn new(name: PackageName, version: Version) -> Self {
        Self { name, version }
    }

    /// Returns the package name for this resolved instance.
    pub fn name(&self) -> &PackageName {
        &self.name
    }

    /// Returns the resolved package version for this instance.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// Returns the directory name for this package in the vendor folder.
    ///
    /// Returns a version-qualified name (`name@version`) when multiple versions
    /// of this package are actually resolved in the graph.
    ///
    /// Note: The `resolver = "multiversion"` setting only grants *permission* to
    /// have multiple versions; it doesn't force version-qualified names when
    /// there's only one version.
    pub fn vendor_dir_name(&self, graph: &DependencyGraph) -> String {
        // Only use version-qualified names when there are actually multiple versions
        if graph.ids_for_name(&self.name).len() > 1 {
            format!("{}@{}", self.name, self.version)
        } else {
            self.name.to_string()
        }
    }
}

impl fmt::Display for ResolvedPackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// Represents a dependency contextualized by the current dependency graph
#[derive(Debug, Clone)]
pub enum ResolvedDependency {
    /// A resolved dependency that is located on a remote registry
    Remote {
        /// The materialized package as downloaded from the registry
        package: Package,
        /// The registry the package was downloaded from
        registry: RegistryRef,
        /// The repository in the registry where the package can be found
        repository: String,
        /// Packages that requested this dependency (and what versions they accept)
        dependants: Vec<Dependant>,
        /// Transitive dependencies
        depends_on: Vec<ResolvedPackageId>,
    },
    /// A resolved dependency that is located on the filesystem
    Local {
        /// The materialized package that was created from the buffrs package at the given path
        package: Package,
        /// Location of the requested package
        path: PathBuf,
        /// Packages that requested this dependency (and what versions they accept)
        dependants: Vec<Dependant>,
        /// Transitive dependencies
        depends_on: Vec<ResolvedPackageId>,
    },
}

impl ResolvedDependency {
    pub(crate) fn package(&self) -> &Package {
        match self {
            Self::Remote { package, .. } => package,
            Self::Local { package, .. } => package,
        }
    }

    pub(crate) fn depends_on(&self) -> &[ResolvedPackageId] {
        match self {
            Self::Remote { depends_on, .. } => depends_on,
            Self::Local { depends_on, .. } => depends_on,
        }
    }

    pub(crate) fn dependants(&self) -> &[Dependant] {
        match self {
            Self::Remote { dependants, .. } => dependants,
            Self::Local { dependants, .. } => dependants,
        }
    }
}

/// Represents a requester of the associated dependency
#[derive(Debug, Clone)]
pub struct Dependant {
    /// Package that requested the dependency
    pub name: PackageName,
    /// Version requirement
    pub version_req: VersionReq,
    /// Whether this edge allows multi-version resolution
    pub allows_multiversion: bool,
    /// Namespace overlap policy for this edge
    pub namespace_overlap_policy: NamespaceOverlapPolicy,
}

/// Represents direct and transitive dependencies of the root package
#[derive(Debug, Clone, Default)]
pub struct DependencyGraph {
    /// Global flag to allow multiple versions (from config, deprecated)
    pub(crate) allow_multiple_versions: bool,
    /// Set of package names that have at least one edge with multiversion permission
    pub(crate) multiversion_permitted: std::collections::HashSet<PackageName>,
    /// Namespace overlap policies per package name (most permissive wins)
    pub(crate) namespace_policies: HashMap<PackageName, NamespaceOverlapPolicy>,
    pub(crate) roots: Vec<ResolvedPackageId>,
    pub(crate) entries: HashMap<ResolvedPackageId, ResolvedDependency>,
}

/// A builder for constructing a dependency graph
pub struct DependencyGraphBuilder<'a> {
    manifest: &'a Manifest,
    lockfile: &'a Lockfile,
    credentials: &'a Credentials,
    cache: &'a Cache,
    config: &'a Config,
    policy: CertValidationPolicy,
    /// Optional shared HTTP client for connection pooling
    client: Option<reqwest::Client>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RemoteDependency {
    package: PackageName,
    manifest: RemoteDependencyManifest,
}

impl From<RemoteDependency> for Dependency {
    fn from(value: RemoteDependency) -> Self {
        Dependency {
            package: value.package,
            manifest: value.manifest.into(),
        }
    }
}

impl Deref for DependencyGraph {
    type Target = HashMap<ResolvedPackageId, ResolvedDependency>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl DerefMut for DependencyGraph {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct LocalDependency {
    package: PackageName,
    manifest: LocalDependencyManifest,
}

#[derive(Error, Diagnostic, Debug)]
#[error("failed to download dependency {name}@{version} from the registry")]
struct DownloadError {
    name: PackageName,
    version: VersionReq,
}

impl DependencyGraph {
    /// Creates a new dependency graph.
    pub fn new(allow_multiple_versions: bool) -> Self {
        Self {
            allow_multiple_versions,
            multiversion_permitted: std::collections::HashSet::new(),
            namespace_policies: HashMap::new(),
            roots: Vec::new(),
            entries: HashMap::new(),
        }
    }

    /// Locates and returns a reference to a resolved dependency package by its id.
    pub fn get(&self, id: &ResolvedPackageId) -> Option<&ResolvedDependency> {
        self.entries.get(id)
    }

    /// Locates and returns a mutable reference to a resolved dependency package by its id.
    pub fn get_mut(&mut self, id: &ResolvedPackageId) -> Option<&mut ResolvedDependency> {
        self.entries.get_mut(id)
    }

    /// Returns the resolved root dependencies corresponding to the root manifest dependencies.
    pub fn roots(&self) -> &[ResolvedPackageId] {
        &self.roots
    }

    /// Returns true if multi-version is globally enabled (deprecated config flag).
    pub fn allow_multiple_versions(&self) -> bool {
        self.allow_multiple_versions
    }

    /// Returns true if multi-version is permitted for a specific package name.
    ///
    /// This checks both the global config flag and per-dependency opt-in.
    pub fn is_multiversion_permitted(&self, name: &PackageName) -> bool {
        self.allow_multiple_versions || self.multiversion_permitted.contains(name)
    }

    /// Returns the effective namespace overlap policy for a package name.
    ///
    /// If multiple edges specify different policies, the most permissive wins.
    pub fn namespace_policy(&self, name: &PackageName) -> NamespaceOverlapPolicy {
        self.namespace_policies
            .get(name)
            .copied()
            .unwrap_or_default()
    }

    /// Grant multi-version permission for a package name.
    pub fn permit_multiversion(&mut self, name: &PackageName) {
        self.multiversion_permitted.insert(name.clone());
    }

    /// Update namespace policy for a package name (most permissive wins).
    pub fn update_namespace_policy(&mut self, name: &PackageName, policy: NamespaceOverlapPolicy) {
        let current = self.namespace_policies.entry(name.clone()).or_default();
        // Most permissive wins: Rewrite > IdenticalOnly > Forbidden
        let new_permissiveness = match policy {
            NamespaceOverlapPolicy::Rewrite => 2,
            NamespaceOverlapPolicy::IdenticalOnly => 1,
            NamespaceOverlapPolicy::Forbidden => 0,
        };
        let current_permissiveness = match *current {
            NamespaceOverlapPolicy::Rewrite => 2,
            NamespaceOverlapPolicy::IdenticalOnly => 1,
            NamespaceOverlapPolicy::Forbidden => 0,
        };
        if new_permissiveness > current_permissiveness {
            *current = policy;
        }
    }

    /// Returns true if the package needs namespace rewriting due to multi-version resolution.
    ///
    /// A package needs rewriting if:
    /// 1. Multiple versions of this package name exist in the graph, AND
    /// 2. The namespace_overlap policy is `Rewrite` (default)
    pub fn needs_namespace_rewrite(&self, id: &ResolvedPackageId) -> bool {
        // Check if multiple versions exist
        let versions_count = self
            .entries
            .keys()
            .filter(|k| k.name() == id.name())
            .count();

        if versions_count <= 1 {
            return false;
        }

        // Check the namespace policy
        let policy = self.namespace_policy(id.name());
        matches!(policy, NamespaceOverlapPolicy::Rewrite)
    }

    /// Returns a list of vendor module directory names used by this graph.
    pub fn vendor_module_names(&self) -> Vec<String> {
        let mut modules: Vec<String> = self
            .entries
            .keys()
            .map(|id| id.vendor_dir_name(self))
            .collect();

        modules.sort();
        modules.dedup();
        modules
    }

    /// Returns all resolved ids for the given package name.
    pub fn ids_for_name(&self, name: &PackageName) -> Vec<ResolvedPackageId> {
        self.entries
            .keys()
            .filter(|id| id.name() == name)
            .cloned()
            .collect()
    }

    /// Returns the only resolved dependency for a package name.
    ///
    /// This is useful for legacy call sites that assume single-version resolution.
    pub fn get_single_by_name(&self, name: &PackageName) -> Option<&ResolvedDependency> {
        let mut matches = self.entries.iter().filter(|(id, _)| id.name() == name);
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.1)
    }
}

impl IntoIterator for DependencyGraph {
    type Item = ResolvedDependency;
    type IntoIter = std::collections::hash_map::IntoValues<ResolvedPackageId, ResolvedDependency>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_values()
    }
}

impl<'a> DependencyGraphBuilder<'a> {
    /// Creates a new dependency graph builder
    ///
    /// # Parameters
    /// - `manifest`: Manifest of the root package
    /// - `lockfile`: Lockfile of the root package
    /// - `credentials`: Credentials used to authenticate with remote registries
    /// - `cache`: Cache used to store downloaded packages
    /// - `config`: Configuration settings
    /// - `policy`: Policy used to validate certificates
    ///
    /// # Returns
    /// A new dependency graph builder
    pub fn new(
        manifest: &'a Manifest,
        lockfile: &'a Lockfile,
        credentials: &'a Credentials,
        cache: &'a Cache,
        config: &'a Config,
        policy: CertValidationPolicy,
    ) -> Self {
        Self {
            manifest,
            lockfile,
            credentials,
            cache,
            config,
            policy,
            client: None,
        }
    }

    /// Sets a shared HTTP client for connection pooling.
    ///
    /// When set, all Artifactory registry operations will reuse this client's
    /// connection pool instead of creating new connections for each request.
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Creates an Artifactory client, reusing the shared HTTP client if available.
    fn create_artifactory(&self, registry: RegistryUri) -> miette::Result<Artifactory> {
        if let Some(ref client) = self.client {
            Artifactory::new_with_client(registry, self.credentials, client.clone())
        } else {
            Artifactory::new(registry, self.credentials, self.policy)
        }
    }

    /// Builds the dependency graph
    pub async fn build(self) -> miette::Result<DependencyGraph> {
        if self.config.use_greedy_resolver() {
            tracing::info!("Using legacy greedy resolver (use_greedy_resolver=true)");
            self.build_greedy().await
        } else {
            self.build_with_pubgrub().await
        }
    }

    /// Builds the dependency graph using greedy version selection (original algorithm)
    async fn build_greedy(self) -> miette::Result<DependencyGraph> {
        let name = self
            .manifest
            .package
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| PackageName::unchecked("."));

        let parent_dir = env::current_dir().into_diagnostic()?;

        // Prepare the dependency graph
        let mut deps = DependencyGraph::new(self.config.allow_multiple_versions());

        let mut roots = Vec::new();

        for dependency in &self.manifest.dependencies {
            let id = self
                .process_dependency(
                    name.clone(),
                    dependency.clone(),
                    true, // is_root
                    &parent_dir,
                    &mut deps,
                )
                .await?;
            roots.push(id);
        }

        deps.roots = roots;

        Ok(deps)
    }

    async fn process_dependency(
        &self,
        name: PackageName,
        dependency: Dependency,
        is_root: bool,
        parent_dir: &Path,
        deps: &mut DependencyGraph,
    ) -> miette::Result<ResolvedPackageId> {
        // Track per-dependency resolver permissions
        if dependency.allows_multiversion() {
            deps.permit_multiversion(&dependency.package);
        }
        deps.update_namespace_policy(&dependency.package, dependency.namespace_overlap_policy());

        let id = match dependency.manifest {
            DependencyManifest::Remote(manifest) => {
                self.process_remote_dependency(
                    name.clone(),
                    RemoteDependency {
                        package: dependency.package,
                        manifest,
                    },
                    is_root,
                    parent_dir,
                    deps,
                )
                .await?
            }
            DependencyManifest::Local(manifest) => {
                self.process_local_dependency(
                    name.clone(),
                    LocalDependency {
                        package: dependency.package,
                        manifest,
                    },
                    is_root,
                    parent_dir,
                    deps,
                )
                .await?
            }
        };

        Ok(id)
    }

    #[async_recursion]
    async fn process_local_dependency(
        &self,
        name: PackageName,
        dependency: LocalDependency,
        is_root: bool,
        parent_dir: &Path,
        deps: &mut DependencyGraph,
    ) -> miette::Result<ResolvedPackageId> {
        // If the dependency.manifest_path is relative, it's relative to the parent manifest.
        // We therefore need to resolve it to an absolute path.
        let abs_manifest_dir = if dependency.manifest.path.is_relative() {
            // combine the parent manifest path with the relative path
            parent_dir
                .join(&dependency.manifest.path)
                .canonicalize()
                .into_diagnostic()
                .wrap_err(miette::miette!(
                    "no `{}` for package {} found at path {} referenced by {} as \"{}\"",
                    MANIFEST_FILE,
                    dependency.package,
                    parent_dir.join(&dependency.manifest.path).display(),
                    name,
                    dependency.manifest.path.display()
                ))?
        } else {
            dependency.manifest.path.clone()
        };

        let manifest =
            Manifest::try_read_from(&abs_manifest_dir.join(MANIFEST_FILE), Some(self.config))
                .await?
                .ok_or_else(|| {
                    miette::miette!(
                        "no `{}` for package {} found at path {} referenced by {} as \"{}\"",
                        MANIFEST_FILE,
                        dependency.package,
                        abs_manifest_dir.join(MANIFEST_FILE).display(),
                        name,
                        dependency.manifest.path.display()
                    )
                })?;

        // Process sub-dependencies first
        let package = if is_root {
            let store = PackageStore::open(&abs_manifest_dir).await?;
            let package = store.release(&manifest, self.config, Some(deps)).await?;

            // Ensure that the package version doesn't clash with an existing entry,
            // and that it matches the version requirement in the manifest
            if let Some(version_req) = dependency
                .manifest
                .publish
                .as_ref()
                .map(|p| p.version.clone())
            {
                let found_version = package.version();

                // Always verify the version requirement against the built package.
                ensure!(
                    version_req.matches(found_version),
                    "a dependency of your project requires {}@{} but the resolved version is {}",
                    package.name(),
                    version_req,
                    found_version,
                );

                // In single-version mode, also ensure no clash with already-resolved entry.
                // Use per-dependency permission if granted, otherwise fall back to global config.
                if !deps.is_multiversion_permitted(package.name()) {
                    if let Some(entry) = deps.get_single_by_name(package.name()) {
                        let existing_package = entry.package();
                        ensure!(
                            version_req.matches(existing_package.version()),
                            "a dependency of your project requires {}@{} which collides with {}@{} required by {:?}",
                            package.name(),
                            found_version,
                            existing_package.name(),
                            existing_package.version(),
                            name,
                        );
                    }
                }
            }

            package
        } else {
            // Non-root packages may not be physically present on disk.
            // Take it from the collected entries instead.
            let mut matches = deps
                .entries
                .iter()
                .filter(|(id, _)| id.name() == &dependency.package);
            let first = matches.next().ok_or_else(|| {
                miette::miette!(
                    "no resolved package found for local dependency {}",
                    dependency.package
                )
            })?;
            ensure!(
                matches.next().is_none(),
                "local dependency {} is ambiguous: multiple resolved versions exist",
                dependency.package
            );
            first.1.package().clone()
        };

        let dependency_id =
            ResolvedPackageId::new(package.name().clone(), package.version().clone());

        // Check if this local dependency is already resolved (same name+version)
        // Extract multiversion settings from the local dependency's publish section
        let allows_multiversion = dependency
            .manifest
            .publish
            .as_ref()
            .map(|p| matches!(p.resolver, ResolverMode::MultiVersion))
            .unwrap_or(false);
        let namespace_overlap_policy = dependency
            .manifest
            .publish
            .as_ref()
            .map(|p| p.namespace_overlap)
            .unwrap_or_default();

        if let Some(existing) = deps.get_mut(&dependency_id) {
            match existing {
                ResolvedDependency::Local { dependants, .. } => {
                    dependants.push(Dependant {
                        name,
                        version_req: VersionReq::STAR,
                        allows_multiversion,
                        namespace_overlap_policy,
                    });
                    return Ok(dependency_id);
                }
                ResolvedDependency::Remote { .. } => {
                    bail!(
                        "a dependency of your project requires local {} but it collides with an already resolved remote dependency",
                        dependency_id
                    );
                }
            }
        }

        // Process the sub-dependencies of the local package and record the resolved ids
        let mut sub_dependency_ids = Vec::new();
        for sub_dependency in package.manifest.dependencies.clone() {
            let sub_id = self
                .process_dependency(
                    package.name().clone(),
                    sub_dependency,
                    false,
                    &abs_manifest_dir,
                    deps,
                )
                .await?;
            sub_dependency_ids.push(sub_id);
        }

        // Add the local package to the dependency graph
        deps.insert(
            dependency_id.clone(),
            ResolvedDependency::Local {
                package,
                path: abs_manifest_dir.clone(),
                dependants: vec![Dependant {
                    name,
                    version_req: VersionReq::STAR,
                    allows_multiversion,
                    namespace_overlap_policy,
                }],
                depends_on: sub_dependency_ids,
            },
        );

        Ok(dependency_id)
    }

    #[async_recursion]
    async fn process_remote_dependency(
        &self,
        name: PackageName,
        dependency: RemoteDependency,
        is_root: bool,
        parent_dir: &Path,
        deps: &mut DependencyGraph,
    ) -> miette::Result<ResolvedPackageId> {
        let version_req = dependency.manifest.version.clone();

        // Check if the dependency is already resolved with a compatible version
        if let Some((existing_id, existing_entry)) = deps
            .entries
            .iter_mut()
            .find(|(id, _)| id.name() == &dependency.package && version_req.matches(id.version()))
        {
            match existing_entry {
                ResolvedDependency::Local {
                    path, dependants, ..
                } => {
                    bail!(
                        "a dependency of your project requires {}@{} which collides with a local dependency for {}@{} required by {:?}",
                        dependency.package,
                        dependency.manifest.version,
                        dependency.package,
                        path.display(),
                        dependants[0].name.clone(),
                    );
                }
                ResolvedDependency::Remote { dependants, .. } => {
                    dependants.push(Dependant {
                        name,
                        version_req,
                        allows_multiversion: matches!(
                            dependency.manifest.resolver,
                            ResolverMode::MultiVersion
                        ),
                        namespace_overlap_policy: dependency.manifest.namespace_overlap,
                    });
                    return Ok(existing_id.clone());
                }
            }
        }

        // If multi-version is disabled for this package, detect name collisions and fail early.
        // Use per-dependency permission if granted, otherwise fall back to global config.
        if !deps.is_multiversion_permitted(&dependency.package) {
            if let Some((existing_id, existing_entry)) = deps
                .entries
                .iter()
                .find(|(id, _)| id.name() == &dependency.package)
            {
                match existing_entry {
                    ResolvedDependency::Remote { package, .. } => {
                        ensure!(
                            version_req.matches(package.version()),
                            "a dependency of your project requires {}@{} which collides with {}@{} required by {:?}",
                            dependency.package,
                            dependency.manifest.version,
                            package.name(),
                            package.version(),
                            existing_entry.dependants()[0].name.clone(),
                        );
                    }
                    ResolvedDependency::Local { path, .. } => {
                        bail!(
                            "a dependency of your project requires {}@{} which collides with a local dependency at {}",
                            dependency.package,
                            dependency.manifest.version,
                            path.display(),
                        );
                    }
                }
                return Ok(existing_id.clone());
            }
        }

        // Resolve the dependency
        let dependency_pkg = self.resolve(dependency.clone(), is_root).await?;

        let dependency_id = ResolvedPackageId::new(
            dependency_pkg.name().clone(),
            dependency_pkg.version().clone(),
        );

        // Process sub-dependencies first and record resolved ids
        let mut sub_dependency_ids = Vec::new();
        for sub_dependency in dependency_pkg.manifest.dependencies.clone() {
            let sub_id = self
                .process_dependency(
                    dependency_pkg.name().clone(),
                    sub_dependency,
                    false,
                    parent_dir,
                    deps,
                )
                .await?;
            sub_dependency_ids.push(sub_id);
        }

        deps.insert(
            dependency_id.clone(),
            ResolvedDependency::Remote {
                package: dependency_pkg,
                registry: dependency.manifest.registry,
                repository: dependency.manifest.repository,
                dependants: vec![Dependant {
                    name,
                    version_req,
                    allows_multiversion: matches!(
                        dependency.manifest.resolver,
                        ResolverMode::MultiVersion
                    ),
                    namespace_overlap_policy: dependency.manifest.namespace_overlap,
                }],
                depends_on: sub_dependency_ids,
            },
        );

        Ok(dependency_id)
    }

    async fn resolve(
        &self,
        dependency: RemoteDependency,
        is_root: bool,
    ) -> miette::Result<Package> {
        let version_req = &dependency.manifest.version;

        // Check if we have a locked version that matches
        if let Some(local_locked) = self.lockfile.find(
            &dependency.package,
            version_req,
            Some(&dependency.manifest.registry),
            Some(&dependency.manifest.repository),
        ) {
            ensure!(
                is_root || dependency.manifest.registry == local_locked.registry,
                "mismatched registry detected for dependency {} - requested {} but lockfile requires {}",
                    dependency.package,
                    dependency.manifest.registry,
                    local_locked.registry,
            );

            // Check cache if locked package matches manifest
            if version_req.matches(&local_locked.version) {
                if let Some(cached) = self.cache.get(local_locked.try_into()?).await? {
                    local_locked.validate(&cached)?;
                    return Ok(cached);
                }
            }

            // Download the locked version
            let registry = self
                .create_artifactory(dependency.manifest.registry.clone().try_into()?)
                .wrap_err(DownloadError {
                    name: dependency.package.clone(),
                    version: dependency.manifest.version.clone(),
                })?;

            let package = registry
                .download_version(
                    &dependency.manifest.repository,
                    &dependency.package,
                    &local_locked.version,
                )
                .await
                .wrap_err(DownloadError {
                    name: dependency.package.clone(),
                    version: dependency.manifest.version.clone(),
                })?;

            let file_requirement = FileRequirement::try_from(local_locked)?;
            self.cache
                .put(file_requirement.into(), package.tgz.clone())
                .await
                .ok();

            Ok(package)
        } else {
            // Package not present in lockfile (and thus not in cache)
            // => resolve version and download from the registry
            let registry = self
                .create_artifactory(dependency.manifest.registry.clone().try_into()?)
                .wrap_err(DownloadError {
                    name: dependency.package.clone(),
                    version: dependency.manifest.version.clone(),
                })?;

            // Resolve version requirement to exact version
            let resolved_version = self
                .resolve_version(&registry, &dependency)
                .await
                .wrap_err(DownloadError {
                    name: dependency.package.clone(),
                    version: dependency.manifest.version.clone(),
                })?;

            tracing::debug!(
                "Resolved {}@{} to version {}",
                dependency.package,
                version_req,
                resolved_version
            );

            let package = registry
                .download_version(
                    &dependency.manifest.repository,
                    &dependency.package,
                    &resolved_version,
                )
                .await
                .wrap_err(DownloadError {
                    name: dependency.package.clone(),
                    version: dependency.manifest.version.clone(),
                })?;

            let key = Entry::from(&package);
            let content = package.tgz.clone();
            self.cache.put(key, content).await.ok();

            Ok(package)
        }
    }

    /// Resolves a version requirement to a specific version.
    ///
    /// If the requirement is already exact, extracts it directly.
    /// Otherwise, queries the registry for available versions and selects the best match.
    async fn resolve_version(
        &self,
        registry: &Artifactory,
        dependency: &RemoteDependency,
    ) -> miette::Result<Version> {
        let version_req = &dependency.manifest.version;

        // Fast path: if version requirement is already exact, extract it
        if is_exact_requirement(version_req) {
            if let Some(exact) = extract_exact_version(version_req) {
                return Ok(exact);
            }
        }

        // Query registry for available versions
        let available = registry
            .list_versions(
                dependency.manifest.repository.clone(),
                dependency.package.clone(),
            )
            .await?;

        if available.is_empty() {
            miette::bail!(
                "no versions of {} found in repository {}",
                dependency.package,
                dependency.manifest.repository
            );
        }

        // Select best matching version
        select_version(version_req, &available).ok_or_else(|| {
            miette::miette!(
                "no version of {} matches requirement '{}' (available: {})",
                dependency.package,
                version_req,
                available
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }

    /// Builds the dependency graph using PubGrub SAT-based resolution.
    ///
    /// This method uses a three-phase approach:
    /// 1. Discovery: Fetch all package metadata from registries (and local manifests)
    /// 2. Resolution: Run PubGrub to find consistent versions
    /// 3. Download: Download packages with resolved versions
    ///
    /// Note: Falls back to greedy resolution when multiversion is required,
    /// since PubGrub inherently produces single-version solutions.
    async fn build_with_pubgrub(self) -> miette::Result<DependencyGraph> {
        // Check if any dependency has multiversion enabled - if so, use greedy
        // PubGrub produces single-version solutions per package, but multiversion
        // allows multiple versions of the same package in the dependency tree
        let has_multiversion = self
            .manifest
            .dependencies
            .iter()
            .any(|dep| match &dep.manifest {
                DependencyManifest::Remote(m) => matches!(m.resolver, ResolverMode::MultiVersion),
                DependencyManifest::Local(m) => m
                    .publish
                    .as_ref()
                    .map(|p| matches!(p.resolver, ResolverMode::MultiVersion))
                    .unwrap_or(false),
            });

        if has_multiversion {
            tracing::debug!("Multiversion dependency detected - using greedy resolution");
            return self.build_greedy().await;
        }

        tracing::debug!("Using PubGrub SAT-based resolution");

        let root_name = self
            .manifest
            .package
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| PackageName::unchecked("."));

        let parent_dir = env::current_dir().into_diagnostic()?;

        // Phase 1: Collect root dependencies and discover package metadata
        let mut root_deps = Vec::new();
        let mut remote_deps_to_discover: VecDeque<(PackageName, RemoteDependencyManifest)> =
            VecDeque::new();
        let mut local_packages: HashMap<PackageName, (Package, PathBuf, LocalDependencyManifest)> =
            HashMap::new();

        // Preferred versions for reproducible installs (from Proto.lock).
        // Applied to the PubGrub provider as a soft preference.
        let mut preferred_versions: HashMap<PackageName, Version> = HashMap::new();

        for dependency in &self.manifest.dependencies {
            match &dependency.manifest {
                DependencyManifest::Remote(manifest) => {
                    root_deps.push(PackageDependency {
                        package: dependency.package.clone(),
                        version_req: manifest.version.clone(),
                    });
                    remote_deps_to_discover
                        .push_back((dependency.package.clone(), manifest.clone()));

                    if let Some(locked) = self.lockfile.find(
                        &dependency.package,
                        &manifest.version,
                        Some(&manifest.registry),
                        Some(&manifest.repository),
                    ) {
                        preferred_versions
                            .entry(dependency.package.clone())
                            .or_insert_with(|| locked.version.clone());
                    }
                }
                DependencyManifest::Local(manifest) => {
                    // Read local package manifest to get its version
                    let abs_manifest_dir = if manifest.path.is_relative() {
                        parent_dir
                            .join(&manifest.path)
                            .canonicalize()
                            .into_diagnostic()
                            .wrap_err(miette::miette!(
                                "local dependency {} not found at path {}",
                                dependency.package,
                                parent_dir.join(&manifest.path).display()
                            ))?
                    } else {
                        manifest.path.clone()
                    };

                    let local_manifest = Manifest::try_read_from(
                        &abs_manifest_dir.join(MANIFEST_FILE),
                        Some(self.config),
                    )
                    .await?
                    .ok_or_else(|| {
                        miette::miette!(
                            "no `{}` for local package {} found at path {}",
                            MANIFEST_FILE,
                            dependency.package,
                            abs_manifest_dir.join(MANIFEST_FILE).display()
                        )
                    })?;

                    // Build the local package
                    let store = PackageStore::open(&abs_manifest_dir).await?;
                    let mut temp_deps = DependencyGraph::new(self.config.allow_multiple_versions());
                    let package = store
                        .release(&local_manifest, self.config, Some(&mut temp_deps))
                        .await?;

                    let version = package.version().clone();

                    // Add version requirement for this local dep (exact version it provides)
                    let version_req = if let Some(publish) = &manifest.publish {
                        publish.version.clone()
                    } else {
                        // If no version specified, create exact requirement
                        VersionReq::parse(&format!("={}", version)).unwrap()
                    };

                    root_deps.push(PackageDependency {
                        package: dependency.package.clone(),
                        version_req: version_req.clone(),
                    });

                    // Queue its remote sub-dependencies for discovery
                    for sub_dep in &package.manifest.dependencies {
                        if let DependencyManifest::Remote(sub_manifest) = &sub_dep.manifest {
                            remote_deps_to_discover
                                .push_back((sub_dep.package.clone(), sub_manifest.clone()));

                            if let Some(locked) = self.lockfile.find(
                                &sub_dep.package,
                                &sub_manifest.version,
                                Some(&sub_manifest.registry),
                                Some(&sub_manifest.repository),
                            ) {
                                preferred_versions
                                    .entry(sub_dep.package.clone())
                                    .or_insert_with(|| locked.version.clone());
                            }
                        }
                    }

                    local_packages.insert(
                        dependency.package.clone(),
                        (package, abs_manifest_dir, manifest.clone()),
                    );
                }
            }
        }

        // Phase 2: Discover all remote packages and their metadata
        let provider = BuffrsDependencyProvider::new(root_deps);
        let mut discovered: HashSet<PackageName> = HashSet::new();

        for (name, version) in preferred_versions {
            provider.set_preferred_version(name, version);
        }

        // Add local packages to provider first (they have exactly one version)
        for (name, (package, _, _)) in &local_packages {
            let version = package.version().clone();

            // Collect dependencies from local package
            let mut pkg_deps = Vec::new();
            for dep in &package.manifest.dependencies {
                match &dep.manifest {
                    DependencyManifest::Remote(dep_manifest) => {
                        pkg_deps.push(PackageDependency {
                            package: dep.package.clone(),
                            version_req: dep_manifest.version.clone(),
                        });
                    }
                    DependencyManifest::Local(dep_manifest) => {
                        // Local sub-dep: use exact version from manifest or star
                        let sub_version_req = dep_manifest
                            .publish
                            .as_ref()
                            .map(|p| p.version.clone())
                            .unwrap_or(VersionReq::STAR);
                        pkg_deps.push(PackageDependency {
                            package: dep.package.clone(),
                            version_req: sub_version_req,
                        });
                    }
                }
            }

            let mut dependencies_map = HashMap::new();
            dependencies_map.insert(version.clone(), pkg_deps);

            provider.add_package(
                name.clone(),
                PackageInfo {
                    versions: vec![version],
                    dependencies: dependencies_map,
                },
            );
            discovered.insert(name.clone());
        }

        // Discover remote packages
        while !remote_deps_to_discover.is_empty() {
            let (pkg_name, manifest) = remote_deps_to_discover
                .pop_front()
                .expect("queue is non-empty");

            if discovered.contains(&pkg_name) {
                continue;
            }
            discovered.insert(pkg_name.clone());

            if let Some(locked) = self.lockfile.find(
                &pkg_name,
                &manifest.version,
                Some(&manifest.registry),
                Some(&manifest.repository),
            ) {
                provider.set_preferred_version(pkg_name.clone(), locked.version.clone());
            }

            tracing::debug!("Discovering package: {}", pkg_name);

            // Create registry client
            let registry = self.create_artifactory(manifest.registry.clone().try_into()?)?;

            // Get all available versions
            let versions = registry
                .list_versions(manifest.repository.clone(), pkg_name.clone())
                .await?;

            if versions.is_empty() {
                miette::bail!(
                    "no versions of {} found in repository {}",
                    pkg_name,
                    manifest.repository
                );
            }

            // Fetch dependencies for each version
            let mut dependencies_map: HashMap<Version, Vec<PackageDependency>> = HashMap::new();

            for version in &versions {
                // Download package to get its manifest
                match registry
                    .download_version(&manifest.repository, &pkg_name, version)
                    .await
                {
                    Ok(package) => {
                        let mut pkg_deps = Vec::new();
                        for dep in &package.manifest.dependencies {
                            match &dep.manifest {
                                DependencyManifest::Remote(dep_manifest) => {
                                    pkg_deps.push(PackageDependency {
                                        package: dep.package.clone(),
                                        version_req: dep_manifest.version.clone(),
                                    });

                                    // Add to discovery queue
                                    if !discovered.contains(&dep.package) {
                                        remote_deps_to_discover
                                            .push_back((dep.package.clone(), dep_manifest.clone()));
                                    }
                                }
                                DependencyManifest::Local(dep_manifest) => {
                                    // Remote package has local dep - use version from manifest
                                    let sub_version_req = dep_manifest
                                        .publish
                                        .as_ref()
                                        .map(|p| p.version.clone())
                                        .unwrap_or(VersionReq::STAR);
                                    pkg_deps.push(PackageDependency {
                                        package: dep.package.clone(),
                                        version_req: sub_version_req,
                                    });
                                }
                            }
                        }
                        dependencies_map.insert(version.clone(), pkg_deps);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to fetch {}@{}: {} - skipping version",
                            pkg_name,
                            version,
                            e
                        );
                    }
                }
            }

            provider.add_package(
                pkg_name,
                PackageInfo {
                    versions: versions.clone(),
                    dependencies: dependencies_map,
                },
            );
        }

        // Phase 3: Run PubGrub resolution
        tracing::debug!(
            "Running PubGrub resolution on {} packages...",
            discovered.len()
        );

        let resolved = crate::pubgrub_resolver::resolve(&provider)
            .map_err(|e| miette::miette!("dependency resolution failed:\n{}", e.message))?;

        tracing::debug!(
            "Resolved {} packages: {}",
            resolved.len(),
            resolved
                .iter()
                .map(|(name, version)| format!("{}@{}", name, version))
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Phase 4: Build dependency graph with resolved versions
        let mut deps = DependencyGraph::new(self.config.allow_multiple_versions());
        let mut roots = Vec::new();

        for dependency in &self.manifest.dependencies {
            match &dependency.manifest {
                DependencyManifest::Remote(manifest) => {
                    // Get the resolved version
                    let resolved_version = resolved.get(&dependency.package).ok_or_else(|| {
                        miette::miette!(
                            "PubGrub did not resolve version for {}",
                            dependency.package
                        )
                    })?;

                    // Download the resolved version
                    let registry =
                        self.create_artifactory(manifest.registry.clone().try_into()?)?;

                    let package = registry
                        .download_version(
                            &manifest.repository,
                            &dependency.package,
                            resolved_version,
                        )
                        .await?;

                    // Cache the package
                    let key = Entry::from(&package);
                    self.cache.put(key, package.tgz.clone()).await.ok();

                    let dependency_id =
                        ResolvedPackageId::new(package.name().clone(), package.version().clone());

                    // Process sub-dependencies recursively
                    let sub_dep_ids = self
                        .build_pubgrub_subdeps(&package, &resolved, &local_packages, &mut deps)
                        .await?;

                    deps.insert(
                        dependency_id.clone(),
                        ResolvedDependency::Remote {
                            package,
                            registry: manifest.registry.clone(),
                            repository: manifest.repository.clone(),
                            dependants: vec![Dependant {
                                name: root_name.clone(),
                                version_req: manifest.version.clone(),
                                allows_multiversion: matches!(
                                    manifest.resolver,
                                    ResolverMode::MultiVersion
                                ),
                                namespace_overlap_policy: manifest.namespace_overlap,
                            }],
                            depends_on: sub_dep_ids,
                        },
                    );

                    roots.push(dependency_id);
                }
                DependencyManifest::Local(manifest) => {
                    let (package, abs_path, _) = local_packages.get(&dependency.package).unwrap();

                    let dependency_id =
                        ResolvedPackageId::new(package.name().clone(), package.version().clone());

                    // Process sub-dependencies recursively
                    let sub_dep_ids = self
                        .build_pubgrub_subdeps(package, &resolved, &local_packages, &mut deps)
                        .await?;

                    // Extract multiversion settings
                    let allows_multiversion = manifest
                        .publish
                        .as_ref()
                        .map(|p| matches!(p.resolver, ResolverMode::MultiVersion))
                        .unwrap_or(false);
                    let namespace_overlap_policy = manifest
                        .publish
                        .as_ref()
                        .map(|p| p.namespace_overlap)
                        .unwrap_or_default();

                    deps.insert(
                        dependency_id.clone(),
                        ResolvedDependency::Local {
                            package: package.clone(),
                            path: abs_path.clone(),
                            dependants: vec![Dependant {
                                name: root_name.clone(),
                                version_req: VersionReq::STAR,
                                allows_multiversion,
                                namespace_overlap_policy,
                            }],
                            depends_on: sub_dep_ids,
                        },
                    );

                    roots.push(dependency_id);
                }
            }
        }

        deps.roots = roots;
        Ok(deps)
    }

    /// Recursively builds sub-dependencies using pre-resolved versions from PubGrub.
    #[async_recursion]
    async fn build_pubgrub_subdeps(
        &self,
        package: &Package,
        resolved: &HashMap<PackageName, Version>,
        local_packages: &HashMap<PackageName, (Package, PathBuf, LocalDependencyManifest)>,
        deps: &mut DependencyGraph,
    ) -> miette::Result<Vec<ResolvedPackageId>> {
        let mut sub_dep_ids = Vec::new();

        for sub_dep in &package.manifest.dependencies {
            match &sub_dep.manifest {
                DependencyManifest::Remote(manifest) => {
                    let resolved_version = resolved.get(&sub_dep.package).ok_or_else(|| {
                        miette::miette!("PubGrub did not resolve version for {}", sub_dep.package)
                    })?;

                    let dep_id =
                        ResolvedPackageId::new(sub_dep.package.clone(), resolved_version.clone());

                    // Check if already processed
                    if let Some(existing) = deps.get_mut(&dep_id) {
                        if let ResolvedDependency::Remote { dependants, .. } = existing {
                            dependants.push(Dependant {
                                name: package.name().clone(),
                                version_req: manifest.version.clone(),
                                allows_multiversion: matches!(
                                    manifest.resolver,
                                    ResolverMode::MultiVersion
                                ),
                                namespace_overlap_policy: manifest.namespace_overlap,
                            });
                        }
                        sub_dep_ids.push(dep_id);
                        continue;
                    }

                    // Download the resolved version
                    let registry =
                        self.create_artifactory(manifest.registry.clone().try_into()?)?;

                    let sub_package = registry
                        .download_version(&manifest.repository, &sub_dep.package, resolved_version)
                        .await?;

                    // Cache it
                    let key = Entry::from(&sub_package);
                    self.cache.put(key, sub_package.tgz.clone()).await.ok();

                    // Recursively process its dependencies
                    let nested_ids = self
                        .build_pubgrub_subdeps(&sub_package, resolved, local_packages, deps)
                        .await?;

                    deps.insert(
                        dep_id.clone(),
                        ResolvedDependency::Remote {
                            package: sub_package,
                            registry: manifest.registry.clone(),
                            repository: manifest.repository.clone(),
                            dependants: vec![Dependant {
                                name: package.name().clone(),
                                version_req: manifest.version.clone(),
                                allows_multiversion: matches!(
                                    manifest.resolver,
                                    ResolverMode::MultiVersion
                                ),
                                namespace_overlap_policy: manifest.namespace_overlap,
                            }],
                            depends_on: nested_ids,
                        },
                    );

                    sub_dep_ids.push(dep_id);
                }
                DependencyManifest::Local(manifest) => {
                    // Check if this local dep was discovered
                    if let Some((local_pkg, local_path, _)) = local_packages.get(&sub_dep.package) {
                        let dep_id = ResolvedPackageId::new(
                            local_pkg.name().clone(),
                            local_pkg.version().clone(),
                        );

                        // Check if already processed
                        if let Some(existing) = deps.get_mut(&dep_id) {
                            if let ResolvedDependency::Local { dependants, .. } = existing {
                                let allows_multiversion = manifest
                                    .publish
                                    .as_ref()
                                    .map(|p| matches!(p.resolver, ResolverMode::MultiVersion))
                                    .unwrap_or(false);
                                let namespace_overlap_policy = manifest
                                    .publish
                                    .as_ref()
                                    .map(|p| p.namespace_overlap)
                                    .unwrap_or_default();

                                dependants.push(Dependant {
                                    name: package.name().clone(),
                                    version_req: VersionReq::STAR,
                                    allows_multiversion,
                                    namespace_overlap_policy,
                                });
                            }
                            sub_dep_ids.push(dep_id);
                            continue;
                        }

                        // Recursively process its dependencies
                        let nested_ids = self
                            .build_pubgrub_subdeps(local_pkg, resolved, local_packages, deps)
                            .await?;

                        let allows_multiversion = manifest
                            .publish
                            .as_ref()
                            .map(|p| matches!(p.resolver, ResolverMode::MultiVersion))
                            .unwrap_or(false);
                        let namespace_overlap_policy = manifest
                            .publish
                            .as_ref()
                            .map(|p| p.namespace_overlap)
                            .unwrap_or_default();

                        deps.insert(
                            dep_id.clone(),
                            ResolvedDependency::Local {
                                package: local_pkg.clone(),
                                path: local_path.clone(),
                                dependants: vec![Dependant {
                                    name: package.name().clone(),
                                    version_req: VersionReq::STAR,
                                    allows_multiversion,
                                    namespace_overlap_policy,
                                }],
                                depends_on: nested_ids,
                            },
                        );

                        sub_dep_ids.push(dep_id);
                    } else {
                        // Local dep wasn't discovered - this is an error
                        miette::bail!(
                            "local dependency {} referenced by {} was not found",
                            sub_dep.package,
                            package.name()
                        );
                    }
                }
            }
        }

        Ok(sub_dep_ids)
    }
}
