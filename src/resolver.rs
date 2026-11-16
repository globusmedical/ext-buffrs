use async_recursion::async_recursion;
use miette::{bail, ensure, Context, Diagnostic, IntoDiagnostic};
use semver::{Version, VersionReq};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fmt,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::Mutex,
};
use thiserror::Error;

use crate::{
    cache::{Cache, Entry},
    config::Config,
    credentials::Credentials,
    lock::{DigestAlgorithm, FileRequirement, LockedPackage, Lockfile},
    manifest::{
        Dependency, DependencyManifest, LocalDependencyManifest, Manifest, NamespaceOverlapPolicy,
        RemoteDependencyManifest, ResolverMode, MANIFEST_FILE,
    },
    package::{Package, PackageName, PackageStore},
    pubgrub_resolver::{
        BuffrsDependencyProvider, PackageDependency, PackageInfo, PackageSource, ResolutionResult,
        SourceAborted, SourcePackage,
    },
    registry::{Artifactory, CertValidationPolicy, DownloadFailure, RegistryRef, RegistryUri},
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
    #[must_use]
    pub fn new(name: PackageName, version: Version) -> Self {
        Self { name, version }
    }

    /// Returns the package name for this resolved instance.
    #[must_use]
    pub fn name(&self) -> &PackageName {
        &self.name
    }

    /// Returns the resolved package version for this instance.
    #[must_use]
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
    #[must_use]
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
    /// Remote packages fetched during this build, so that no version is downloaded twice.
    fetched: Mutex<HashMap<FetchKey, Package>>,
    /// Install exactly what `Proto.lock` pins, failing where it cannot be used as is.
    locked: bool,
}

/// Registry URL, repository, package name and version of a fetched remote package.
type FetchKey = (String, String, PackageName, Version);

/// What the remote packages of a `PubGrub` build are fetched from, kept across resolution passes.
#[derive(Default)]
struct Discovery {
    /// Manifest of each remote package, as first named by the root or a fetched package
    manifests: HashMap<PackageName, RemoteDependencyManifest>,
    /// Versions listed by the registry for each package
    listings: HashMap<PackageName, Vec<Version>>,
    /// Whether locked packages offer their locked version as the only candidate
    use_lock: bool,
    /// Number of packages offering their locked version in the current pass
    locked_candidates: usize,
    /// Registry and repository of every manifest that names each remote package
    requested: HashMap<PackageName, Vec<(RegistryRef, String)>>,
    /// The `Proto.lock` entry offered as the only candidate of each package in the current pass
    locked_entries: HashMap<PackageName, LockedPackage>,
}

impl Discovery {
    /// Records a manifest naming a remote package. The first one is the package's manifest.
    fn request(&mut self, name: &PackageName, manifest: &RemoteDependencyManifest) {
        let requested = self.requested.entry(name.clone()).or_default();
        let source = (manifest.registry.clone(), manifest.repository.clone());
        if !requested.contains(&source) {
            requested.push(source);
        }

        self.manifests
            .entry(name.clone())
            .or_insert_with(|| manifest.clone());
    }

    /// The dependencies of a fetched remote package, remembering the manifests of its remote
    /// dependencies.
    fn dependencies_of(&mut self, package: &Package) -> Vec<PackageDependency> {
        package
            .manifest
            .dependencies
            .iter()
            .map(|dependency| match &dependency.manifest {
                DependencyManifest::Remote(manifest) => {
                    self.request(&dependency.package, manifest);

                    PackageDependency {
                        package: dependency.package.clone(),
                        version_req: manifest.version.clone(),
                    }
                }
                DependencyManifest::Local(manifest) => PackageDependency {
                    package: dependency.package.clone(),
                    version_req: manifest
                        .publish
                        .as_ref()
                        .map_or(VersionReq::STAR, |publish| publish.version.clone()),
                },
            })
            .collect()
    }
}

/// A metadata request from the resolution thread, answered by the async build.
enum SourceRequest {
    Versions(PackageName, Reply<Option<SourcePackage>>),
    Dependencies(PackageName, Version, Reply<Option<Vec<PackageDependency>>>),
}

type Reply<T> = std::sync::mpsc::Sender<Result<T, SourceAborted>>;

/// A [`PackageSource`] that forwards each request to the async build and blocks until it replies.
struct ChannelSource(tokio::sync::mpsc::UnboundedSender<SourceRequest>);

impl ChannelSource {
    fn ask<T>(&self, request: impl FnOnce(Reply<T>) -> SourceRequest) -> Result<T, SourceAborted> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.0.send(request(reply)).map_err(|_| SourceAborted)?;
        answer.recv().map_err(|_| SourceAborted)?
    }
}

impl PackageSource for ChannelSource {
    fn versions(&self, name: &PackageName) -> Result<Option<SourcePackage>, SourceAborted> {
        self.ask(|reply| SourceRequest::Versions(name.clone(), reply))
    }

    fn dependencies(
        &self,
        name: &PackageName,
        version: &Version,
    ) -> Result<Option<Vec<PackageDependency>>, SourceAborted> {
        self.ask(|reply| SourceRequest::Dependencies(name.clone(), version.clone(), reply))
    }
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
    #[must_use]
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
    #[must_use]
    pub fn get(&self, id: &ResolvedPackageId) -> Option<&ResolvedDependency> {
        self.entries.get(id)
    }

    /// Locates and returns a mutable reference to a resolved dependency package by its id.
    pub fn get_mut(&mut self, id: &ResolvedPackageId) -> Option<&mut ResolvedDependency> {
        self.entries.get_mut(id)
    }

    /// Returns the resolved root dependencies corresponding to the root manifest dependencies.
    #[must_use]
    pub fn roots(&self) -> &[ResolvedPackageId] {
        &self.roots
    }

    /// Returns true if multi-version is globally enabled (deprecated config flag).
    #[must_use]
    pub fn allow_multiple_versions(&self) -> bool {
        self.allow_multiple_versions
    }

    /// Returns true if multi-version is permitted for a specific package name.
    ///
    /// This checks both the global config flag and per-dependency opt-in.
    #[must_use]
    pub fn is_multiversion_permitted(&self, name: &PackageName) -> bool {
        self.allow_multiple_versions || self.multiversion_permitted.contains(name)
    }

    /// Returns the effective namespace overlap policy for a package name.
    ///
    /// If multiple edges specify different policies, the most permissive wins.
    #[must_use]
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
    /// 2. The `namespace_overlap` policy is `Rewrite` (default)
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
            fetched: Mutex::new(HashMap::new()),
            locked: false,
        }
    }

    /// Makes the build use exactly what `Proto.lock` pins.
    ///
    /// The build then fails, naming the package, when a remote dependency has no matching lock
    /// entry, when the lock entry's repository is not one the manifests request the package
    /// from, when the locked versions do not satisfy the manifests, when a locked artifact is
    /// missing from the registry and from the cache, or when a downloaded artifact differs from
    /// the digest in `Proto.lock`.
    #[must_use]
    pub fn locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    /// Sets a shared HTTP client for connection pooling.
    ///
    /// When set, all Artifactory registry operations will reuse this client's
    /// connection pool instead of creating new connections for each request.
    #[must_use]
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
            .map_or_else(|| PackageName::unchecked("."), |p| p.name.clone());

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
            let package = store
                .release(&manifest, self.config, Some(deps), false)
                .await?;

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
                            version_req,
                            existing_package.name(),
                            existing_package.version(),
                            entry.dependants(),
                        );
                    }
                }
            }

            package
        } else {
            // Non-root packages: check if already resolved, otherwise load from disk.
            let mut matches = deps
                .entries
                .iter()
                .filter(|(id, _)| id.name() == &dependency.package);
            if let Some(first) = matches.next() {
                ensure!(
                    matches.next().is_none(),
                    "local dependency {} is ambiguous: multiple resolved versions exist",
                    dependency.package
                );

                let package = first.1.package();

                // Verify version requirement if a publish section exists
                if let Some(version_req) = dependency
                    .manifest
                    .publish
                    .as_ref()
                    .map(|p| p.version.clone())
                {
                    let found_version = package.version();
                    ensure!(
                        version_req.matches(found_version),
                        "a dependency of your project requires {}@{} but the resolved version is {}",
                        package.name(),
                        version_req,
                        found_version,
                    );
                }

                package.clone()
            } else {
                // Transitive local dependency not yet resolved — load from disk.
                let store = PackageStore::open(&abs_manifest_dir).await?;
                let package = store
                    .release(&manifest, self.config, Some(deps), false)
                    .await?;

                // Verify version requirement if a publish section exists
                if let Some(version_req) = dependency
                    .manifest
                    .publish
                    .as_ref()
                    .map(|p| p.version.clone())
                {
                    let found_version = package.version();
                    ensure!(
                        version_req.matches(found_version),
                        "a dependency of your project requires {}@{} but the resolved version is {}",
                        package.name(),
                        version_req,
                        found_version,
                    );

                    // In single-version mode, also ensure no clash with already-resolved entry.
                    if !deps.is_multiversion_permitted(package.name()) {
                        if let Some(entry) = deps.get_single_by_name(package.name()) {
                            let existing_package = entry.package();
                            ensure!(
                                version_req.matches(existing_package.version()),
                                "a dependency of your project requires {}@{} which collides with {}@{} required by {:?}",
                                package.name(),
                                version_req,
                                existing_package.name(),
                                existing_package.version(),
                                entry.dependants(),
                            );
                        }
                    }
                }

                package
            }
        };

        let dependency_id =
            ResolvedPackageId::new(package.name().clone(), package.version().clone());

        // Check if this local dependency is already resolved (same name+version)
        // Extract multiversion settings from the local dependency's publish section
        let allows_multiversion = dependency
            .manifest
            .publish
            .as_ref()
            .is_some_and(|p| matches!(p.resolver, ResolverMode::MultiVersion));
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
                is_root || dependency.manifest.registry.same_registry(&local_locked.registry),
                "mismatched registry detected for dependency {} - requested {} but lockfile requires {}",
                    dependency.package,
                    dependency.manifest.registry,
                    local_locked.registry,
            );

            // Check cache if locked package matches manifest. An entry that cannot be read, or
            // whose content differs from the locked digest, is downloaded again.
            if version_req.matches(&local_locked.version) {
                let cached = self
                    .cache
                    .get(local_locked.try_into()?)
                    .await
                    .ok()
                    .flatten()
                    .filter(|cached| local_locked.validate(cached).is_ok());

                if let Some(cached) = cached {
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

            let package = match registry
                .try_download_version(
                    &dependency.manifest.repository,
                    &dependency.package,
                    &local_locked.version,
                )
                .await
            {
                Ok(package) => package,
                Err(DownloadFailure::NotFound(_)) if self.locked => {
                    return Err(Self::missing_locked_artifact(
                        &dependency.package,
                        &local_locked.version,
                        &dependency.manifest.repository,
                    ));
                }
                Err(e) => {
                    return Err(e.into_report()).wrap_err(DownloadError {
                        name: dependency.package.clone(),
                        version: dependency.manifest.version.clone(),
                    })
                }
            };

            self.check_digest(local_locked, &package, &dependency.manifest.repository)?;

            let file_requirement = FileRequirement::try_from(local_locked)?;
            self.cache
                .put(file_requirement.into(), package.tgz.clone())
                .await
                .ok();

            Ok(package)
        } else {
            if self.locked {
                return Err(Self::not_locked(&dependency.package, version_req));
            }

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
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }

    /// Builds the dependency graph using `PubGrub` SAT-based resolution.
    ///
    /// This method uses a three-phase approach:
    /// 1. Discovery: Fetch all package metadata from registries (and local manifests)
    /// 2. Resolution: Run `PubGrub` to find consistent versions
    /// 3. Download: Download packages with resolved versions
    ///
    /// Note: Falls back to greedy resolution when multiversion is required,
    /// since `PubGrub` inherently produces single-version solutions.
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
                    .is_some_and(|p| matches!(p.resolver, ResolverMode::MultiVersion)),
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
            .map_or_else(|| PackageName::unchecked("."), |p| p.name.clone());

        let parent_dir = env::current_dir().into_diagnostic()?;

        // Phase 1: Collect root dependencies and discover package metadata
        let mut root_deps = Vec::new();
        let mut remote_deps_to_discover: VecDeque<(PackageName, RemoteDependencyManifest)> =
            VecDeque::new();
        let mut local_deps_to_discover: VecDeque<(PackageName, LocalDependencyManifest, PathBuf)> =
            VecDeque::new();
        let mut queued_local: HashSet<PackageName> = HashSet::new();
        let mut local_packages: HashMap<PackageName, (Package, PathBuf, LocalDependencyManifest)> =
            HashMap::new();

        for dependency in &self.manifest.dependencies {
            match &dependency.manifest {
                DependencyManifest::Remote(manifest) => {
                    root_deps.push(PackageDependency {
                        package: dependency.package.clone(),
                        version_req: manifest.version.clone(),
                    });
                    remote_deps_to_discover
                        .push_back((dependency.package.clone(), manifest.clone()));
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
                    let temp_deps = DependencyGraph::new(self.config.allow_multiple_versions());
                    let package = store
                        .release(&local_manifest, self.config, Some(&temp_deps), false)
                        .await?;

                    let version = package.version().clone();

                    // Add version requirement for this local dep (exact version it provides)
                    let version_req = if let Some(publish) = &manifest.publish {
                        publish.version.clone()
                    } else {
                        // If no version specified, create exact requirement
                        VersionReq::parse(&format!("={version}")).unwrap()
                    };

                    root_deps.push(PackageDependency {
                        package: dependency.package.clone(),
                        version_req: version_req.clone(),
                    });

                    // Queue its sub-dependencies for discovery
                    for sub_dep in &package.manifest.dependencies {
                        match &sub_dep.manifest {
                            DependencyManifest::Remote(sub_manifest) => {
                                remote_deps_to_discover
                                    .push_back((sub_dep.package.clone(), sub_manifest.clone()));
                            }
                            DependencyManifest::Local(sub_manifest) => {
                                // Queue transitive local deps for discovery
                                if !local_packages.contains_key(&sub_dep.package)
                                    && queued_local.insert(sub_dep.package.clone())
                                {
                                    local_deps_to_discover.push_back((
                                        sub_dep.package.clone(),
                                        sub_manifest.clone(),
                                        abs_manifest_dir.clone(),
                                    ));
                                }
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

        // Discover transitive local dependencies
        while let Some((pkg_name, manifest, parent_path)) = local_deps_to_discover.pop_front() {
            if local_packages.contains_key(&pkg_name) {
                continue;
            }

            let manifest_dir = if manifest.path.is_relative() {
                parent_path.join(&manifest.path)
            } else {
                manifest.path.clone()
            };

            let abs_manifest_dir =
                manifest_dir
                    .canonicalize()
                    .into_diagnostic()
                    .wrap_err(miette::miette!(
                        "local dependency {} not found at path {}",
                        pkg_name,
                        manifest_dir.display()
                    ))?;

            let local_manifest =
                Manifest::try_read_from(&abs_manifest_dir.join(MANIFEST_FILE), Some(self.config))
                    .await?
                    .ok_or_else(|| {
                        miette::miette!(
                            "no `{}` for local package {} found at path {}",
                            MANIFEST_FILE,
                            pkg_name,
                            abs_manifest_dir.join(MANIFEST_FILE).display()
                        )
                    })?;

            let store = PackageStore::open(&abs_manifest_dir).await?;
            let temp_deps = DependencyGraph::new(self.config.allow_multiple_versions());
            let package = store
                .release(&local_manifest, self.config, Some(&temp_deps), false)
                .await?;

            // Queue its sub-dependencies
            for sub_dep in &package.manifest.dependencies {
                match &sub_dep.manifest {
                    DependencyManifest::Remote(sub_manifest) => {
                        remote_deps_to_discover
                            .push_back((sub_dep.package.clone(), sub_manifest.clone()));
                    }
                    DependencyManifest::Local(sub_manifest) => {
                        if !local_packages.contains_key(&sub_dep.package)
                            && queued_local.insert(sub_dep.package.clone())
                        {
                            local_deps_to_discover.push_back((
                                sub_dep.package.clone(),
                                sub_manifest.clone(),
                                abs_manifest_dir.clone(),
                            ));
                        }
                    }
                }
            }

            local_packages.insert(pkg_name, (package, abs_manifest_dir, manifest));
        }

        // Remote packages are listed and downloaded only when PubGrub first needs them, from the
        // registry and repository of the first manifest that names them.
        let mut discovery = Discovery::default();
        for (name, manifest) in &remote_deps_to_discover {
            discovery.request(name, manifest);
        }

        // Resolution runs once with the locked versions as the only candidates, which needs no
        // registry listing, and again against the full registry if those versions no longer
        // satisfy the manifests.
        let mut use_lock = true;
        let resolved = loop {
            discovery.use_lock = use_lock;
            discovery.locked_candidates = 0;
            discovery.locked_entries.clear();

            let provider = BuffrsDependencyProvider::new(root_deps.clone());

            // Local packages have exactly one version
            for (name, (package, _, _)) in &local_packages {
                let version = package.version().clone();

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
                                .map_or(VersionReq::STAR, |p| p.version.clone());
                            pkg_deps.push(PackageDependency {
                                package: dep.package.clone(),
                                version_req: sub_version_req,
                            });
                        }
                    }
                }

                provider.add_package(
                    name.clone(),
                    PackageInfo {
                        versions: vec![version.clone()],
                        dependencies: HashMap::from([(version, pkg_deps)]),
                    },
                );
            }

            tracing::debug!("Running PubGrub resolution");

            match self.resolve_lazily(provider, &mut discovery).await? {
                Ok(resolved) => break resolved,
                Err(e) if use_lock && discovery.locked_candidates > 0 => {
                    if self.locked {
                        bail!(
                            "Proto.lock does not satisfy the manifests, and a locked install cannot resolve them again:\n{}",
                            e.message
                        );
                    }

                    tracing::info!(
                        "Proto.lock does not satisfy the manifests ({}), resolving against the registry",
                        e.message
                    );
                    use_lock = false;
                }
                Err(e) => bail!("dependency resolution failed:\n{}", e.message),
            }
        };

        if self.locked {
            Self::check_locked_repositories(&discovery)?;
        }

        tracing::debug!(
            "Resolved {} packages: {}",
            resolved.len(),
            resolved
                .iter()
                .map(|(name, version)| format!("{name}@{version}"))
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

                    let package = self
                        .fetch_remote(&dependency.package, manifest, resolved_version)
                        .await?;

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
                        .is_some_and(|p| matches!(p.resolver, ResolverMode::MultiVersion));
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

    /// Runs `PubGrub` on a blocking thread while this task fetches the package metadata it asks
    /// for, one request at a time.
    ///
    /// Returns the outcome of the resolution, or the error that stopped a fetch.
    async fn resolve_lazily(
        &self,
        provider: BuffrsDependencyProvider,
        discovery: &mut Discovery,
    ) -> miette::Result<ResolutionResult> {
        let (requests, mut pending) = tokio::sync::mpsc::unbounded_channel();
        let provider = provider.with_source(ChannelSource(requests));
        let resolution =
            tokio::task::spawn_blocking(move || crate::pubgrub_resolver::resolve(&provider));

        // The channel closes when the resolution ends and drops the provider.
        let mut failure: Option<miette::Report> = None;
        while let Some(request) = pending.recv().await {
            match request {
                SourceRequest::Versions(name, reply) => {
                    let outcome = if failure.is_some() {
                        Err(SourceAborted)
                    } else {
                        self.serve_versions(&name, discovery).await.map_err(|e| {
                            failure = Some(e);
                            SourceAborted
                        })
                    };
                    reply.send(outcome).ok();
                }
                SourceRequest::Dependencies(name, version, reply) => {
                    let outcome = if failure.is_some() {
                        Err(SourceAborted)
                    } else {
                        self.serve_dependencies(&name, &version, discovery)
                            .await
                            .map_err(|e| {
                                failure = Some(e);
                                SourceAborted
                            })
                    };
                    reply.send(outcome).ok();
                }
            }
        }

        let resolved = match resolution.await {
            Ok(resolved) => resolved,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => return Err(e).into_diagnostic(),
        };

        match failure {
            Some(e) => Err(e),
            None => Ok(resolved),
        }
    }

    /// The candidate versions of a remote package.
    ///
    /// While `Proto.lock` is in use, a locked package offers its locked version as the only
    /// candidate, taken from the cache or a single download, so the registry is not listed.
    async fn serve_versions(
        &self,
        name: &PackageName,
        discovery: &mut Discovery,
    ) -> miette::Result<Option<SourcePackage>> {
        let Some(manifest) = discovery.manifests.get(name).cloned() else {
            return Ok(None);
        };

        tracing::debug!("Discovering package: {}", name);

        let locked = self
            .lockfile
            .find(name, &manifest.version, Some(&manifest.registry), None);

        if discovery.use_lock {
            match locked {
                Some(locked) => {
                    let package = self
                        .try_fetch_remote(name, &manifest, &locked.version)
                        .await
                        .wrap_err(DownloadError {
                            name: name.clone(),
                            version: manifest.version.clone(),
                        })?;

                    if let Some(package) = package {
                        let version = locked.version.clone();
                        let dependencies = discovery.dependencies_of(&package);
                        discovery.locked_candidates += 1;
                        discovery
                            .locked_entries
                            .insert(name.clone(), locked.clone());

                        return Ok(Some(SourcePackage {
                            info: PackageInfo {
                                versions: vec![version.clone()],
                                dependencies: HashMap::from([(version.clone(), dependencies)]),
                            },
                            preferred: Some(version),
                        }));
                    }

                    if self.locked {
                        return Err(Self::missing_locked_artifact(
                            name,
                            &locked.version,
                            &manifest.repository,
                        ));
                    }

                    tracing::warn!(
                        "{}@{} is pinned by Proto.lock but no longer in repository {}, resolving it against the registry",
                        name,
                        locked.version,
                        manifest.repository
                    );
                }
                None if self.locked => return Err(Self::not_locked(name, &manifest.version)),
                None => {}
            }
        }

        let versions = match discovery.listings.get(name) {
            Some(versions) => versions.clone(),
            None => {
                let registry = self.create_artifactory(manifest.registry.clone().try_into()?)?;
                let versions = registry
                    .list_versions(manifest.repository.clone(), name.clone())
                    .await?;

                if versions.is_empty() {
                    bail!(
                        "no versions of {} found in repository {}",
                        name,
                        manifest.repository
                    );
                }

                discovery.listings.insert(name.clone(), versions.clone());
                versions
            }
        };

        Ok(Some(SourcePackage {
            info: PackageInfo {
                versions,
                dependencies: HashMap::new(),
            },
            preferred: locked.map(|locked| locked.version.clone()),
        }))
    }

    /// The dependencies of a remote package version, or `None` if the registry lacks it.
    async fn serve_dependencies(
        &self,
        name: &PackageName,
        version: &Version,
        discovery: &mut Discovery,
    ) -> miette::Result<Option<Vec<PackageDependency>>> {
        let Some(manifest) = discovery.manifests.get(name).cloned() else {
            return Ok(None);
        };

        // Skipping a version the registry fails to serve for now would resolve to another one
        // and rewrite Proto.lock, so only a missing version is skipped.
        let package = self
            .try_fetch_remote(name, &manifest, version)
            .await
            .wrap_err(DownloadError {
                name: name.clone(),
                version: manifest.version.clone(),
            })?;

        let Some(package) = package else {
            tracing::warn!(
                "{}@{} is listed in repository {} but cannot be downloaded - skipping version",
                name,
                version,
                manifest.repository
            );
            return Ok(None);
        };

        Ok(Some(discovery.dependencies_of(&package)))
    }

    /// Returns the given version of a remote package, or an error if the registry lacks it.
    async fn fetch_remote(
        &self,
        name: &PackageName,
        manifest: &RemoteDependencyManifest,
        version: &Version,
    ) -> miette::Result<Package> {
        self.try_fetch_remote(name, manifest, version)
            .await?
            .ok_or_else(|| {
                miette::miette!(
                    "{name}@{version} not found in repository {}",
                    manifest.repository
                )
            })
    }

    /// Returns the given version of a remote package, or `None` if the registry lacks it.
    ///
    /// A version fetched earlier in this build is returned from memory. Otherwise the cache is
    /// read when `Proto.lock` pins the version: the cache is keyed by digest, which only the
    /// lockfile knows before a download. Everything else is downloaded and stored in the cache.
    async fn try_fetch_remote(
        &self,
        name: &PackageName,
        manifest: &RemoteDependencyManifest,
        version: &Version,
    ) -> miette::Result<Option<Package>> {
        let key = Self::fetch_key(manifest, name, version);

        if let Some(package) = self.fetched.lock().unwrap().get(&key) {
            return Ok(Some(package.clone()));
        }

        let locked = VersionReq::parse(&format!("={version}"))
            .ok()
            .and_then(|exact| {
                self.lockfile
                    .find(name, &exact, Some(&manifest.registry), None)
            })
            .filter(|locked| &locked.version == version);

        if let Some(locked) = locked {
            let cached = match FileRequirement::try_from(locked) {
                Ok(requirement) => self.cache.get(requirement).await.ok().flatten(),
                Err(_) => None,
            };

            if let Some(cached) = cached.filter(|cached| locked.validate(cached).is_ok()) {
                tracing::debug!("Using cached {}@{}", name, version);
                self.fetched.lock().unwrap().insert(key, cached.clone());
                return Ok(Some(cached));
            }
        }

        let registry = self.create_artifactory(manifest.registry.clone().try_into()?)?;

        let package = match registry
            .try_download_version(&manifest.repository, name, version)
            .await
        {
            Ok(package) => package,
            Err(DownloadFailure::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e.into_report()),
        };

        if let Some(locked) = locked {
            self.check_digest(locked, &package, &manifest.repository)?;
        }

        self.remember(manifest, &package).await;

        Ok(Some(package))
    }

    /// Checks a downloaded package against the digest in `Proto.lock`.
    ///
    /// A mismatch fails a locked build and is a warning otherwise.
    fn check_digest(
        &self,
        locked: &LockedPackage,
        package: &Package,
        repository: &str,
    ) -> miette::Result<()> {
        if locked.validate(package).is_ok() {
            return Ok(());
        }

        if self.locked {
            bail!(
                "{}@{} in repository {} has digest {}, but Proto.lock pins digest {}",
                locked.name,
                locked.version,
                repository,
                package.digest(DigestAlgorithm::SHA256),
                locked.digest
            );
        }

        tracing::warn!(
            "{}@{} in repository {} differs from the digest in Proto.lock",
            locked.name,
            locked.version,
            repository
        );

        Ok(())
    }

    /// The error of a locked build for a dependency that `Proto.lock` does not pin.
    fn not_locked(name: &PackageName, version_req: &VersionReq) -> miette::Report {
        miette::miette!(
            "{name} {version_req} has no matching entry in Proto.lock, and a locked install cannot add it"
        )
    }

    /// Fails a locked build when `Proto.lock` pins a package to a repository that no manifest
    /// naming the package requests.
    ///
    /// `Proto.lock` records one repository per package while manifests may request it from
    /// several, so the lock entry is found without its repository and checked here, once every
    /// manifest naming the package is known.
    fn check_locked_repositories(discovery: &Discovery) -> miette::Result<()> {
        let mut locked_entries: Vec<_> = discovery.locked_entries.iter().collect();
        locked_entries.sort_by(|a, b| a.0.cmp(b.0));

        for (name, locked) in locked_entries {
            let mut repositories: Vec<&str> = discovery
                .requested
                .get(name)
                .into_iter()
                .flatten()
                .filter(|(registry, _)| registry.same_registry(&locked.registry))
                .map(|(_, repository)| repository.as_str())
                .collect();

            if repositories.contains(&locked.repository.as_str()) {
                continue;
            }

            repositories.sort_unstable();
            repositories.dedup();
            let requested = match repositories.as_slice() {
                [repository] => format!("repository {repository}"),
                repositories => format!("repositories {}", repositories.join(", ")),
            };

            bail!(
                "{name}@{} is locked to repository {} in Proto.lock, but the manifests request it from {requested}, and a locked install cannot change its repository",
                locked.version,
                locked.repository
            );
        }

        Ok(())
    }

    /// The error of a locked build for a pinned version that cannot be fetched.
    fn missing_locked_artifact(
        name: &PackageName,
        version: &Version,
        repository: &str,
    ) -> miette::Report {
        miette::miette!(
            "{name}@{version} is pinned by Proto.lock but is missing from repository {repository} and from the cache"
        )
    }

    /// Stores a downloaded package in the cache and in this build's memory.
    async fn remember(&self, manifest: &RemoteDependencyManifest, package: &Package) {
        if let Err(e) = self
            .cache
            .put(Entry::from(package), package.tgz.clone())
            .await
        {
            tracing::debug!(
                "could not cache {}@{}: {e}",
                package.name(),
                package.version()
            );
        }

        let key = Self::fetch_key(manifest, package.name(), package.version());
        self.fetched.lock().unwrap().insert(key, package.clone());
    }

    fn fetch_key(
        manifest: &RemoteDependencyManifest,
        name: &PackageName,
        version: &Version,
    ) -> FetchKey {
        let registry = manifest
            .registry
            .url()
            .map_or_else(|| manifest.registry.to_string(), ToString::to_string);

        (
            registry,
            manifest.repository.clone(),
            name.clone(),
            version.clone(),
        )
    }

    /// Recursively builds sub-dependencies using pre-resolved versions from `PubGrub`.
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

                    let sub_package = self
                        .fetch_remote(&sub_dep.package, manifest, resolved_version)
                        .await?;

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
                                let allows_multiversion =
                                    manifest.publish.as_ref().is_some_and(|p| {
                                        matches!(p.resolver, ResolverMode::MultiVersion)
                                    });
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
                            .is_some_and(|p| matches!(p.resolver, ResolverMode::MultiVersion));
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
