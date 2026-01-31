use async_recursion::async_recursion;
use miette::{bail, ensure, Context, Diagnostic, IntoDiagnostic};
use semver::{Version, VersionReq};
use std::{
    collections::HashMap,
    env,
    fmt,
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
        Dependency, DependencyManifest, LocalDependencyManifest, Manifest,
        NamespaceOverlapPolicy, RemoteDependencyManifest, ResolverMode, MANIFEST_FILE,
    },
    package::{Package, PackageName, PackageStore},
    registry::{Artifactory, CertValidationPolicy, RegistryRef},
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
    pub fn new(name: PackageName, version: Version) -> Self {
        Self { name, version }
    }

    pub fn name(&self) -> &PackageName {
        &self.name
    }

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
        // Most permissive wins: Allowed > IdenticalOnly > Forbidden
        let new_permissiveness = match policy {
            NamespaceOverlapPolicy::Allowed => 2,
            NamespaceOverlapPolicy::IdenticalOnly => 1,
            NamespaceOverlapPolicy::Forbidden => 0,
        };
        let current_permissiveness = match *current {
            NamespaceOverlapPolicy::Allowed => 2,
            NamespaceOverlapPolicy::IdenticalOnly => 1,
            NamespaceOverlapPolicy::Forbidden => 0,
        };
        if new_permissiveness > current_permissiveness {
            *current = policy;
        }
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
        }
    }

    /// Builds the dependency graph
    pub async fn build(self) -> miette::Result<DependencyGraph> {
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
            if let Some(version_req) = dependency.manifest.publish.map(|p| p.version) {
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

        let dependency_id = ResolvedPackageId::new(package.name().clone(), package.version().clone());

        // Check if this local dependency is already resolved (same name+version)
        if let Some(existing) = deps.get_mut(&dependency_id) {
            match existing {
                ResolvedDependency::Local { dependants, .. } => {
                    dependants.push(Dependant {
                        name,
                        version_req: VersionReq::STAR,
                        allows_multiversion: false,
                        namespace_overlap_policy: NamespaceOverlapPolicy::Forbidden,
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
                    allows_multiversion: false,
                    namespace_overlap_policy: NamespaceOverlapPolicy::Forbidden,
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
                ResolvedDependency::Local { path, dependants, .. } => {
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
                        allows_multiversion: matches!(dependency.manifest.resolver, ResolverMode::MultiVersion),
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
                    allows_multiversion: matches!(dependency.manifest.resolver, ResolverMode::MultiVersion),
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
        if let Some(local_locked) = self.lockfile.find(
            &dependency.package,
            &dependency.manifest.version,
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

            // For now we should only check cache if locked package matches manifest,
            // but theoretically we should be able to still look into cache when freshly installing
            // a dependency.
            if dependency.manifest.version.matches(&local_locked.version) {
                if let Some(cached) = self.cache.get(local_locked.try_into()?).await? {
                    local_locked.validate(&cached)?;
                    return Ok(cached);
                }
            }

            let registry = Artifactory::new(
                dependency.manifest.registry.clone().try_into()?,
                self.credentials,
                self.policy,
            )
            .wrap_err(DownloadError {
                name: dependency.package.clone(),
                version: dependency.manifest.version.clone(),
            })?;

            let package = registry
                // TODO(#205): This works now because buffrs only supports pinned versions.
                // This logic has to change once we implement dynamic version resolution.
                .download(dependency.clone().into())
                .await
                .wrap_err(DownloadError {
                    name: dependency.package,
                    version: dependency.manifest.version,
                })?;

            let file_requirement = FileRequirement::try_from(local_locked)?;
            self.cache
                .put(file_requirement.into(), package.tgz.clone())
                .await
                .ok();

            Ok(package)
        } else {
            // Package not present in lockfile (and thus not in cache)
            // => download it from the registry
            let registry = Artifactory::new(
                dependency.manifest.registry.clone().try_into()?,
                self.credentials,
                self.policy,
            )
            .wrap_err(DownloadError {
                name: dependency.package.clone(),
                version: dependency.manifest.version.clone(),
            })?;

            let package = registry
                .download(dependency.clone().into())
                .await
                .wrap_err(DownloadError {
                    name: dependency.package,
                    version: dependency.manifest.version,
                })?;

            let key = Entry::from(&package);
            let content = package.tgz.clone();
            self.cache.put(key, content).await.ok();

            Ok(package)
        }
    }
}
