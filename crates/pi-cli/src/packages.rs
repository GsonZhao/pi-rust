//! Discovery of Pi-compatible package resources.
//!
//! This module resolves package manifests and static resource paths. Extension
//! paths are handed to the Node bridge by `js_extensions`; the Rust cdylib
//! loader remains a separate extension mechanism. A package is a directory containing a
//! `package.json` (or a conventional `skills/`, `prompts/`, `themes/` tree).
//! The optional `pi`/`rpi` manifest object may override those resource paths.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config;

const PACKAGE_SOURCE_MARKER: &str = ".rpi-package-source.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRoot {
    pub root: PathBuf,
    pub name: String,
    pub version: Option<String>,
    pub manifest: Option<PathBuf>,
    /// Original settings entry used to resolve this package.
    pub spec: String,
    /// Provenance used by update checks. Managed-directory placement alone is
    /// not enough to prove that a package came from the npm registry.
    pub source: PackageSource,
    /// Native Pi/npm-managed install root when this package lives below an
    /// owned `npm/node_modules` tree. These packages must be updated through
    /// the package manager at the root so its manifest and lockfile stay in
    /// sync; only rpi's standalone package roots may use leaf-directory swaps.
    npm_install_root: Option<PathBuf>,
    /// Canonical `node_modules` ancestor that validated a legacy global
    /// install. This is discovery-only provenance: callers must migrate the
    /// package into an rpi/native managed root before any update.
    legacy_npm_root: Option<PathBuf>,
    /// Project `autoload:false` entries are deltas over a matching user entry.
    /// Keep the marker so scope merging can retain both sides of the delta.
    autoload_delta: bool,
    scope: ResolveScope,
    git_store_root: Option<PathBuf>,
    git_revision: Option<String>,
    /// Configured source whose managed checkout is absent. These records are
    /// emitted only while planning remediation so the caller can restore the
    /// installation without exposing nonexistent resources as loadable files.
    missing_install: bool,
    filter: Option<crate::settings::PackageFilter>,
    skills: Vec<PathBuf>,
    prompts: Vec<PathBuf>,
    themes: Vec<PathBuf>,
    system_prompts: Vec<PathBuf>,
    append_system_prompts: Vec<PathBuf>,
    /// JavaScript/TypeScript extension entry files discovered from
    /// `pi.extensions`/`rpi.extensions` or the conventional directory.
    pub extensions: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSource {
    Npm {
        name: String,
        spec: String,
        /// Configured version, range, tag, or one-level `npm:` alias target.
        requested: Option<String>,
        /// Only an exact semantic version is pinned. Ranges and tags update.
        pinned: bool,
    },
    Git,
    Local,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedNpmPackageSpec {
    /// Dependency key and on-disk `node_modules` slot.
    pub(crate) install_name: String,
    /// Name required in the installed package's manifest. This differs from
    /// `install_name` only for one-level registry aliases.
    pub(crate) manifest_name: String,
    /// Version, range, tag, or the complete `npm:<target>` alias selector.
    pub(crate) requested: Option<String>,
    /// Version, range, or tag applied to the package named by `manifest_name`.
    /// For aliases this strips the outer `npm:<target>` portion so runtime
    /// compatibility checks compare the installed target version correctly.
    pub(crate) target_selector: Option<String>,
    pub(crate) is_alias: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDiagnostic {
    pub spec: String,
    pub message: String,
    pub blocks_update: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PackageResources {
    pub packages: Vec<PackageRoot>,
    pub diagnostics: Vec<PackageDiagnostic>,
}

impl PackageResources {
    pub fn extension_paths(&self) -> Vec<PathBuf> {
        self.packages
            .iter()
            .flat_map(|p| p.extensions.iter().cloned())
            .collect()
    }
    pub fn skill_dirs(&self) -> Vec<PathBuf> {
        self.packages
            .iter()
            .flat_map(|p| p.skills.iter().cloned())
            .collect()
    }

    pub fn prompt_dirs(&self) -> Vec<PathBuf> {
        self.packages
            .iter()
            .flat_map(|p| p.prompts.iter().cloned())
            .collect()
    }

    pub fn theme_files(&self) -> Vec<PathBuf> {
        self.packages
            .iter()
            .flat_map(|p| resource_inventory(&p.root, &p.themes, FilterResourceKind::Themes))
            .collect()
    }

    pub fn system_prompt_files(&self) -> Vec<PathBuf> {
        self.packages
            .iter()
            .flat_map(|p| p.system_prompts.iter().cloned())
            .collect()
    }

    pub fn append_system_prompt_files(&self) -> Vec<PathBuf> {
        self.packages
            .iter()
            .flat_map(|p| p.append_system_prompts.iter().cloned())
            .collect()
    }

    pub fn find_theme(&self, name: &str) -> Option<PathBuf> {
        let wanted = Path::new(name);
        self.theme_files().into_iter().find(|path| {
            path == wanted
                || path.file_stem().and_then(|s| s.to_str()) == Some(name)
                || path.file_name().and_then(|s| s.to_str()) == Some(name)
        })
    }
}

/// Resolve package specs from the settings file and conventional local roots.
/// Empty or missing `packages` means no packages are enabled, matching Pi's
/// explicit package list instead of silently executing every directory found
/// under the user's home directory.
pub fn discover_from_settings(cwd: &Path) -> PackageResources {
    discover_configured_packages(cwd, false, true)
}

fn discover_from_settings_for_update(
    cwd: &Path,
    project_trusted: bool,
) -> Result<(PackageResources, Option<crate::npm::NpmCommand>), String> {
    let project_settings = if project_trusted {
        crate::settings::load_active_project_settings(cwd)
            .map_err(|error| format!("could not load project package settings: {error}"))?
            .map(|(_, settings)| settings)
    } else {
        None
    };
    let user_settings = crate::settings::load_settings()
        .map_err(|error| format!("could not load global package settings: {error}"))?;
    let project_specs = project_settings
        .as_ref()
        .and_then(|settings| settings.packages.as_deref())
        .unwrap_or_default();
    let user_specs = user_settings.packages.as_deref().unwrap_or_default();
    let has_configured_packages = !project_specs.is_empty() || !user_specs.is_empty();
    let npm_command = if has_configured_packages {
        let configured = project_settings
            .as_ref()
            .and_then(|settings| settings.npm_command.as_deref())
            .or(user_settings.npm_command.as_deref());
        Some(
            crate::npm::NpmCommand::from_argv(configured).map_err(|error| {
                format!("invalid npmCommand in active package settings: {error}")
            })?,
        )
    } else {
        None
    };
    let resources = discover_configured_package_specs_for_update_with_command(
        cwd,
        project_specs,
        user_specs,
        npm_command.as_ref(),
    );
    Ok((resources, npm_command))
}

fn discover_configured_packages(
    cwd: &Path,
    recover_for_update: bool,
    include_project: bool,
) -> PackageResources {
    let project_specs = include_project
        .then(|| {
            crate::settings::load_project_settings(cwd)
                .into_iter()
                .filter_map(|settings| settings.packages)
                .flatten()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let user_specs = crate::settings::load_settings()
        .ok()
        .and_then(|settings| settings.packages)
        .unwrap_or_default();
    discover_configured_package_specs(
        cwd,
        &project_specs,
        &user_specs,
        recover_for_update,
        include_project,
    )
}

fn discover_configured_package_specs(
    cwd: &Path,
    project_specs: &[crate::settings::PackageSetting],
    user_specs: &[crate::settings::PackageSetting],
    recover_for_update: bool,
    include_project: bool,
) -> PackageResources {
    let npm_command = crate::npm::NpmCommand::resolve(cwd, include_project).ok();
    discover_configured_package_specs_with_command(
        cwd,
        project_specs,
        user_specs,
        recover_for_update,
        npm_command.as_ref(),
    )
}

fn discover_configured_package_specs_with_command(
    cwd: &Path,
    project_specs: &[crate::settings::PackageSetting],
    user_specs: &[crate::settings::PackageSetting],
    recover_for_update: bool,
    npm_command: Option<&crate::npm::NpmCommand>,
) -> PackageResources {
    let project = discover_with_scope_and_command(
        cwd,
        project_specs,
        ResolveScope::Project,
        recover_for_update,
        npm_command,
    );
    let user = discover_with_scope_and_command(
        cwd,
        user_specs,
        ResolveScope::User,
        recover_for_update,
        npm_command,
    );
    merge_scoped_resources([project, user])
}

fn discover_configured_package_specs_for_update_with_command(
    cwd: &Path,
    project_specs: &[crate::settings::PackageSetting],
    user_specs: &[crate::settings::PackageSetting],
    npm_command: Option<&crate::npm::NpmCommand>,
) -> PackageResources {
    let project = discover_with_scope_and_command(
        cwd,
        project_specs,
        ResolveScope::Project,
        true,
        npm_command,
    );
    let user =
        discover_with_scope_and_command(cwd, user_specs, ResolveScope::User, true, npm_command);
    // Runtime resolution is project-first for an identity collision, but the
    // update command must reconcile both physical installations. Native Pi's
    // update() likewise queues global and project settings independently.
    let mut combined = PackageResources::default();
    for mut resources in [project, user] {
        combined.packages.append(&mut resources.packages);
        combined.diagnostics.append(&mut resources.diagnostics);
    }
    combined
}

/// Resolve packages for the explicitly enabled JS/TS runtime. Unlike the
/// metadata-only discovery helpers, this mirrors native Pi by restoring a
/// missing npm/Git source and reconciling an installed npm version that no
/// longer satisfies its configured semver range. Callers must already have
/// passed the project trust gate before selecting the project variant.
pub fn resolve_from_settings(cwd: &Path) -> PackageResources {
    resolve_configured_packages_for_runtime(cwd, true)
}

/// Runtime resolution restricted to global settings. This is used when the
/// current project is not trusted, so no project command or storage path is
/// read or touched.
pub fn resolve_from_global_settings(cwd: &Path) -> PackageResources {
    resolve_configured_packages_for_runtime(cwd, false)
}

/// Offline runtime resolution never invokes npm or Git. Installed npm
/// packages that do not satisfy their configured version are withheld rather
/// than executing stale code, matching native Pi's offline missing-source
/// behavior.
pub fn resolve_offline_from_settings(cwd: &Path) -> PackageResources {
    resolve_configured_packages_offline(cwd, true)
}

pub fn resolve_offline_from_global_settings(cwd: &Path) -> PackageResources {
    resolve_configured_packages_offline(cwd, false)
}

fn resolve_configured_packages_offline(cwd: &Path, include_project: bool) -> PackageResources {
    let project_specs = include_project
        .then(|| {
            crate::settings::load_project_settings(cwd)
                .into_iter()
                .filter_map(|settings| settings.packages)
                .flatten()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let user_specs = crate::settings::load_settings()
        .ok()
        .and_then(|settings| settings.packages)
        .unwrap_or_default();
    let mut resources = discover_configured_package_specs_with_command(
        cwd,
        &project_specs,
        &user_specs,
        false,
        None,
    );
    let failures = runtime_npm_mismatch_failures(
        &resources,
        "configured npm version is unavailable while offline",
    );
    apply_runtime_failures(&mut resources, failures);
    resources
}

fn resolve_configured_packages_for_runtime(cwd: &Path, include_project: bool) -> PackageResources {
    let project_specs = include_project
        .then(|| {
            crate::settings::load_project_settings(cwd)
                .into_iter()
                .filter_map(|settings| settings.packages)
                .flatten()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let user_specs = crate::settings::load_settings()
        .ok()
        .and_then(|settings| settings.packages)
        .unwrap_or_default();
    let planned =
        discover_configured_package_specs(cwd, &project_specs, &user_specs, true, include_project);

    // Store enough identity alongside each operation to prevent a failed or
    // partially completed package-manager command from exposing the old,
    // incompatible package to the JS runtime.
    let mut npm_roots: BTreeMap<PathBuf, Vec<(String, String, String)>> = BTreeMap::new();
    let mut standalone_npm = Vec::new();
    let mut missing_git = Vec::new();
    let mut planning_failures: HashMap<String, (String, String)> = HashMap::new();

    for package in &planned.packages {
        if runtime_npm_needs_install(package) {
            let PackageSource::Npm { name, spec, .. } = &package.source else {
                unreachable!();
            };
            let identity = package_identity(package);
            match package.npm_store_root_for_update(cwd, include_project) {
                Ok(Some(root)) => {
                    npm_roots
                        .entry(root)
                        .or_default()
                        .push((name.clone(), spec.clone(), identity))
                }
                Ok(None) => standalone_npm.push((
                    package.root.clone(),
                    package.name.clone(),
                    name.clone(),
                    spec.clone(),
                    identity,
                )),
                Err(error) => {
                    planning_failures.insert(identity, (package.spec.clone(), error));
                }
            }
        } else if package.missing_install && matches!(package.source, PackageSource::Git) {
            missing_git.push(package.clone());
        }
    }

    if npm_roots.is_empty()
        && standalone_npm.is_empty()
        && missing_git.is_empty()
        && planning_failures.is_empty()
    {
        return planned;
    }

    let mut failures = planning_failures;
    match crate::npm::NpmCommand::resolve(cwd, include_project) {
        Ok(npm_command) => {
            for (root, display_name, name, source_spec, identity) in standalone_npm {
                if let Err(error) = crate::install_pi::update_npm_package_for_startup(
                    &root,
                    &name,
                    &source_spec,
                    &npm_command,
                ) {
                    failures.insert(
                        identity,
                        (
                            source_spec,
                            format!("could not restore npm package {display_name}: {error}"),
                        ),
                    );
                }
            }
            for (root, packages) in npm_roots {
                let install_specs = packages
                    .iter()
                    .map(|(name, source, _)| (name.clone(), source.clone()))
                    .collect::<Vec<_>>();
                if let Err(error) = crate::install_pi::update_npm_store_root_for_startup(
                    &root,
                    &install_specs,
                    &npm_command,
                    cwd,
                    include_project,
                ) {
                    for (_, source, identity) in packages {
                        failures.insert(
                            identity,
                            (source, format!("could not restore npm package: {error}")),
                        );
                    }
                }
            }
            for package in missing_git {
                let identity = package_identity(&package);
                if let Err(error) = crate::install_pi::install_missing_git_package_for_startup(
                    cwd,
                    package.scope == ResolveScope::User,
                    &package.spec,
                    &npm_command,
                ) {
                    failures.insert(
                        identity,
                        (
                            package.spec.clone(),
                            format!("could not restore git package: {error}"),
                        ),
                    );
                }
            }
        }
        Err(error) => {
            for (_, source, identity) in npm_roots.into_values().flatten() {
                failures.insert(identity, (source, error.clone()));
            }
            for (_, _, _, source, identity) in standalone_npm {
                failures.insert(identity, (source, error.clone()));
            }
            for package in missing_git {
                failures.insert(package_identity(&package), (package.spec, error.clone()));
            }
        }
    }

    let mut resolved =
        discover_configured_package_specs(cwd, &project_specs, &user_specs, false, include_project);
    failures.extend(runtime_npm_mismatch_failures(
        &resolved,
        "package manager completed but the installed npm version still does not satisfy settings",
    ));
    if failures.is_empty() {
        return resolved;
    }
    apply_runtime_failures(&mut resolved, failures);
    resolved
}

fn runtime_npm_mismatch_failures(
    resources: &PackageResources,
    message: &str,
) -> HashMap<String, (String, String)> {
    resources
        .packages
        .iter()
        .filter(|package| runtime_npm_needs_install(package))
        .map(|package| {
            (
                package_identity(package),
                (package.spec.clone(), message.to_string()),
            )
        })
        .collect()
}

fn apply_runtime_failures(
    resources: &mut PackageResources,
    failures: HashMap<String, (String, String)>,
) {
    if failures.is_empty() {
        return;
    }
    resources
        .packages
        .retain(|package| !failures.contains_key(&package_identity(package)));
    resources.diagnostics.retain(|diagnostic| {
        !failures
            .values()
            .any(|(source, _)| source == &diagnostic.spec)
    });
    resources.diagnostics.extend(
        failures
            .into_values()
            .map(|(spec, message)| PackageDiagnostic {
                spec,
                message,
                blocks_update: true,
            }),
    );
}

fn runtime_npm_needs_install(package: &PackageRoot) -> bool {
    let PackageSource::Npm { spec, .. } = &package.source else {
        return false;
    };
    if package.missing_install {
        return true;
    }
    let Some(requested) = parse_npm_package_spec(spec).and_then(|parsed| parsed.target_selector)
    else {
        return false;
    };
    npm_version_matches_requirement(package.version.as_deref(), &requested) == Some(false)
}

/// Return `None` for npm tags or range syntax the Rust semver parser cannot
/// represent. Native Pi does not version-check tags, and treating an unknown
/// range as satisfied avoids a reinstall loop while retaining exact, caret,
/// tilde, wildcard, comparator, OR, and hyphen range support.
fn npm_version_matches_requirement(installed: Option<&str>, requested: &str) -> Option<bool> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    if let Some((left, right)) = requested.split_once("||") {
        let mut recognized = false;
        for branch in std::iter::once(left).chain(right.split("||")) {
            if let Some(matches) = npm_version_matches_requirement(installed, branch) {
                recognized = true;
                if matches {
                    return Some(true);
                }
            }
        }
        return recognized.then_some(false);
    }
    let installed = installed
        .and_then(|version| semver::Version::parse(version.trim().trim_start_matches('v')).ok());
    if is_exact_npm_version(requested) {
        let expected = semver::Version::parse(requested.trim_start_matches('v')).ok()?;
        return Some(installed.as_ref() == Some(&expected));
    }
    if let Some((minimum, maximum)) = requested.split_once(" - ") {
        let (minimum, _) = parse_npm_partial_version(minimum)?;
        let (maximum, maximum_parts) = parse_npm_partial_version(maximum)?;
        return Some(installed.as_ref().is_some_and(|installed| {
            let below_upper = match maximum_parts {
                1 => maximum
                    .major
                    .checked_add(1)
                    .is_some_and(|major| installed < &semver::Version::new(major, 0, 0)),
                2 => maximum.minor.checked_add(1).is_some_and(|minor| {
                    installed < &semver::Version::new(maximum.major, minor, 0)
                }),
                _ => installed <= &maximum,
            };
            installed >= &minimum && below_upper
        }));
    }

    let tokens = requested.split_whitespace().collect::<Vec<_>>();
    let normalized = normalize_npm_comparator_set(&tokens).unwrap_or_else(|| requested.to_string());
    // Bare partial versions have npm semantics that differ from Cargo's
    // caret-default syntax. Express their upper bound explicitly.
    if let Some((partial, parts)) = parse_npm_partial_version(requested) {
        if parts < 3 {
            return if parts == 1 {
                Some(
                    installed
                        .as_ref()
                        .is_some_and(|version| version.major == partial.major),
                )
            } else {
                Some(installed.as_ref().is_some_and(|version| {
                    version.major == partial.major && version.minor == partial.minor
                }))
            };
        }
    }
    semver::VersionReq::parse(&normalized)
        .ok()
        .map(|requirement| {
            installed
                .as_ref()
                .is_some_and(|installed| requirement.matches(installed))
        })
}

/// Cargo's semver parser requires comma-separated comparators and does not
/// accept npm's optional whitespace between an operator and its version.
/// Normalize only a conservative comparator set; tags and other npm-only
/// syntax continue to return `None` instead of being guessed at.
fn normalize_npm_comparator_set(tokens: &[&str]) -> Option<String> {
    if tokens.len() <= 1 {
        return None;
    }
    let mut normalized = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        if matches!(token, "<" | "<=" | ">" | ">=" | "=" | "^" | "~") {
            let version = *tokens.get(index + 1)?;
            if !version
                .trim_start_matches('v')
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
            {
                return None;
            }
            normalized.push(format!("{token}{version}"));
            index += 2;
            continue;
        }
        if !token.chars().next().is_some_and(|character| {
            matches!(character, '<' | '>' | '=' | '^' | '~') || character.is_ascii_digit()
        }) {
            return None;
        }
        normalized.push(token.to_string());
        index += 1;
    }
    Some(normalized.join(", "))
}

fn parse_npm_partial_version(value: &str) -> Option<(semver::Version, usize)> {
    let value = value.trim().trim_start_matches('v');
    if let Ok(version) = semver::Version::parse(value) {
        return Some((version, 3));
    }
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.is_empty()
        || parts.len() > 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }
    let major = parts[0].parse().ok()?;
    let minor = parts.get(1).and_then(|part| part.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|part| part.parse().ok()).unwrap_or(0);
    Some((semver::Version::new(major, minor, patch), parts.len()))
}

fn merge_scoped_resources(
    resources: impl IntoIterator<Item = PackageResources>,
) -> PackageResources {
    let mut merged = PackageResources::default();
    let mut seen_identities: HashMap<String, usize> = HashMap::new();
    for mut resource in resources {
        merged.diagnostics.append(&mut resource.diagnostics);
        for package in resource.packages {
            let identity = package_identity(&package);
            if let Some(existing_index) = seen_identities.get(&identity).copied() {
                let existing = &merged.packages[existing_index];
                // Native Pi keeps a project autoload:false entry as a delta
                // over the matching global package. All other collisions are
                // project-first (the resources iterator is project then user).
                if package.scope == ResolveScope::User && existing.autoload_delta {
                    let mut base = package;
                    if let Some(filter) = existing.filter.as_ref() {
                        apply_autoload_delta_to_package(&mut base, filter);
                    }
                    merged.packages[existing_index] = base;
                }
            } else {
                seen_identities.insert(identity, merged.packages.len());
                merged.packages.push(package);
            }
        }
    }
    merged
}

/// Resolve only packages declared in the global settings file. Project-local
/// package declarations are intentionally excluded when the current project
/// has not been trusted.
pub fn discover_from_global_settings(cwd: &Path) -> PackageResources {
    let specs = crate::settings::load_settings()
        .ok()
        .and_then(|settings| settings.packages)
        .unwrap_or_default();
    discover_with_scope(cwd, &specs, ResolveScope::User, false)
}

/// Discover packages in settings order. Package resources are intentionally
/// returned after project and global resources; callers append these paths last
/// so a package cannot shadow a project-local or user-local resource.
pub fn discover(cwd: &Path, specs: &[String]) -> PackageResources {
    let entries = specs
        .iter()
        .cloned()
        .map(crate::settings::PackageSetting::from)
        .collect::<Vec<_>>();
    discover_with_scope(cwd, &entries, ResolveScope::Any, false)
}

fn discover_with_scope(
    cwd: &Path,
    specs: &[crate::settings::PackageSetting],
    scope: ResolveScope,
    recover_for_update: bool,
) -> PackageResources {
    let global_npm_command = if matches!(scope, ResolveScope::Any | ResolveScope::User) {
        crate::npm::NpmCommand::resolve(cwd, false).ok()
    } else {
        None
    };
    discover_with_scope_and_command(
        cwd,
        specs,
        scope,
        recover_for_update,
        global_npm_command.as_ref(),
    )
}

fn discover_with_scope_and_command(
    cwd: &Path,
    specs: &[crate::settings::PackageSetting],
    scope: ResolveScope,
    recover_for_update: bool,
    global_npm_command: Option<&crate::npm::NpmCommand>,
) -> PackageResources {
    let mut out = PackageResources::default();
    let mut seen = HashSet::new();
    let legacy_npm_names = specs
        .iter()
        .filter_map(|entry| npm_source_from_spec(entry.source()))
        .filter_map(|source| match source {
            PackageSource::Npm { name, .. } => Some(name),
            _ => None,
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut legacy_npm_paths = None;
    for entry in specs
        .iter()
        .filter(|entry| !entry.source().trim().is_empty())
    {
        let spec = entry.source();
        let filter = match entry {
            crate::settings::PackageSetting::Filtered(filter) => Some(filter),
            crate::settings::PackageSetting::Source(_) => None,
        };
        if recover_for_update {
            let mut recovery_failed = false;
            for target in update_recovery_targets(cwd, spec, scope) {
                if let Err(message) = crate::install_pi::recover_configured_package_root(&target) {
                    out.diagnostics.push(PackageDiagnostic {
                        spec: spec.to_string(),
                        message,
                        blocks_update: true,
                    });
                    recovery_failed = true;
                    break;
                }
                if target.is_dir() {
                    break;
                }
            }
            if recovery_failed {
                continue;
            }
        }
        let Some(resolved) = resolve_spec_with_command(
            cwd,
            spec,
            scope,
            global_npm_command,
            &legacy_npm_names,
            &mut legacy_npm_paths,
        ) else {
            if recover_for_update {
                match missing_package_for_update(cwd, spec, scope, filter) {
                    Ok(Some(package)) => {
                        let key = package_identity(&package);
                        if seen.insert(key) {
                            out.packages.push(package);
                        }
                        continue;
                    }
                    // Native Pi's manual update ignores local sources. A
                    // missing local path therefore does not turn an otherwise
                    // valid package update into a failure.
                    Ok(None) => continue,
                    Err(message) => {
                        out.diagnostics.push(PackageDiagnostic {
                            spec: spec.to_string(),
                            message,
                            blocks_update: true,
                        });
                        continue;
                    }
                }
            }
            out.diagnostics.push(PackageDiagnostic {
                spec: spec.to_string(),
                message: "package path/name could not be resolved".to_string(),
                blocks_update: true,
            });
            continue;
        };
        let root = resolved.root;
        let key = normalize_key(&root);
        if !seen.insert(key) {
            continue;
        }
        match load_package_with_legacy_root(
            root,
            spec,
            cwd,
            scope,
            filter,
            resolved.legacy_npm_root,
        ) {
            Ok(package) => out.packages.push(package),
            Err(message) => out.diagnostics.push(PackageDiagnostic {
                spec: spec.to_string(),
                message,
                blocks_update: true,
            }),
        }
    }
    out
}

fn missing_package_for_update(
    cwd: &Path,
    spec: &str,
    scope: ResolveScope,
    filter: Option<&crate::settings::PackageFilter>,
) -> Result<Option<PackageRoot>, String> {
    let (root, name, source, npm_install_root, git_store_root, git_revision) =
        if let Some(source @ PackageSource::Npm { .. }) = npm_source_from_spec(spec) {
            let PackageSource::Npm { name, .. } = &source else {
                unreachable!();
            };
            let name = name.clone();
            let install_root = managed_npm_root_for_scope(cwd, scope)?;
            let root = install_root.join("node_modules").join(&name);
            (root, name, source, Some(install_root), None, None)
        } else if let Some(git) = parse_git_source(spec) {
            let store_root = managed_git_root_for_scope(cwd, scope)?;
            let root = store_root.join(&git.host).join(&git.path);
            let name = git
                .path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(&git.path)
                .to_string();
            (
                root,
                name,
                PackageSource::Git,
                None,
                Some(store_root),
                git.revision,
            )
        } else {
            return Ok(None);
        };

    Ok(Some(PackageRoot {
        root,
        name,
        version: None,
        manifest: None,
        spec: spec.to_string(),
        source,
        npm_install_root,
        legacy_npm_root: None,
        autoload_delta: filter.is_some_and(|filter| filter.autoload == Some(false)),
        scope,
        git_store_root,
        git_revision,
        missing_install: true,
        filter: filter.cloned(),
        skills: Vec::new(),
        prompts: Vec::new(),
        themes: Vec::new(),
        system_prompts: Vec::new(),
        append_system_prompts: Vec::new(),
        extensions: Vec::new(),
    }))
}

fn managed_npm_root_for_scope(cwd: &Path, scope: ResolveScope) -> Result<PathBuf, String> {
    match scope {
        ResolveScope::Project => Ok(cwd.join(".pi/npm")),
        ResolveScope::User | ResolveScope::Any => config::agent_dir()
            .map(|agent| agent.join("npm"))
            .map_err(|error| error.to_string()),
    }
}

fn managed_git_root_for_scope(cwd: &Path, scope: ResolveScope) -> Result<PathBuf, String> {
    match scope {
        ResolveScope::Project => Ok(cwd.join(".pi/git")),
        ResolveScope::User | ResolveScope::Any => config::agent_dir()
            .map(|agent| agent.join("git"))
            .map_err(|error| error.to_string()),
    }
}

fn absolute_file_spec_path(spec: &str) -> Option<PathBuf> {
    let path = PathBuf::from(spec.strip_prefix("file:")?);
    path.is_absolute().then_some(path)
}

fn update_recovery_targets(cwd: &Path, spec: &str, scope: ResolveScope) -> Vec<PathBuf> {
    let mut targets = absolute_file_spec_path(spec)
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(git) = parse_git_source(spec) {
        let relative = Path::new(&git.host).join(&git.path);
        if matches!(scope, ResolveScope::Any | ResolveScope::Project) {
            targets.push(cwd.join(".rpi/git").join(&relative));
            targets.push(cwd.join(".pi/git").join(&relative));
        }
        if matches!(scope, ResolveScope::Any | ResolveScope::User) {
            if let Ok(agent) = config::agent_dir() {
                targets.push(agent.join("git").join(&relative));
            }
            if let Some(home) = dirs::home_dir() {
                targets.push(home.join(".pi/agent/git").join(&relative));
            }
        }
        return targets;
    }
    let Some(PackageSource::Npm { name, .. }) = npm_source_from_spec(spec) else {
        return targets;
    };
    let package_key = name.strip_prefix('@').unwrap_or(&name).replace('/', "__");
    if matches!(scope, ResolveScope::Any | ResolveScope::Project) {
        targets.push(cwd.join(".rpi/packages").join(&name));
        if package_key != name {
            targets.push(cwd.join(".rpi/packages").join(&package_key));
        }
        targets.push(cwd.join(".pi/packages").join(&name));
        if package_key != name {
            targets.push(cwd.join(".pi/packages").join(&package_key));
        }
        targets.push(cwd.join(".pi/npm/node_modules").join(&name));
    }
    if matches!(scope, ResolveScope::Any | ResolveScope::User) {
        if let Ok(agent) = config::agent_dir() {
            targets.push(agent.join("packages").join(&name));
            if package_key != name {
                targets.push(agent.join("packages").join(&package_key));
            }
            targets.push(agent.join("npm/node_modules").join(&name));
        }
        if let Some(home) = dirs::home_dir() {
            targets.push(home.join(".pi/agent/packages").join(&name));
            if package_key != name {
                targets.push(home.join(".pi/agent/packages").join(&package_key));
            }
            targets.push(home.join(".pi/agent/npm/node_modules").join(&name));
        }
    }
    targets
}

/// Validate and load one package spec. Used by `rpi package add` before the
/// spec is persisted to settings.
pub fn resolve_package(cwd: &Path, spec: &str) -> Result<PackageRoot, String> {
    let root = resolve_spec(cwd, spec, ResolveScope::Any)
        .ok_or_else(|| "package path/name could not be resolved".to_string())?;
    load_package(root, spec, cwd, ResolveScope::Any, None)
}

/// Load a theme from an explicit JSON path without discovering configured Pi
/// packages. Startup code that has passed the package gate uses
/// [`load_theme_with_resources`] to resolve package theme names.
pub fn load_theme(cwd: &Path, name_or_path: &str) -> Result<rpi_tui::Theme, String> {
    load_theme_with_resources(cwd, name_or_path, &PackageResources::default())
}

/// Load a package theme from an already-resolved resource set. Startup callers
/// use this variant so a disabled package configuration cannot be re-discovered
/// indirectly from a TUI theme selector.
pub fn load_theme_with_resources(
    _cwd: &Path,
    name_or_path: &str,
    resources: &PackageResources,
) -> Result<rpi_tui::Theme, String> {
    let path = {
        let direct = PathBuf::from(name_or_path);
        if direct.is_file() {
            Some(direct)
        } else {
            resources.find_theme(name_or_path)
        }
    }
    .ok_or_else(|| format!("theme `{name_or_path}` was not found in enabled packages"))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read theme {}: {error}", path.display()))?;
    let value = parse_json_with_comments(&text)
        .map_err(|error| format!("invalid theme {}: {error}", path.display()))?;
    let mut theme = rpi_tui::Theme::default();
    let colors = value.get("colors").unwrap_or(&value);
    let target = &mut theme.colors;
    macro_rules! color {
        ($field:ident, $($key:literal),+ $(,)?) => {
            if let Some(value) = first_value(colors, &[$($key),+]) {
                if let Some(parsed) = parse_color(value) {
                    target.$field = parsed;
                }
            }
        };
    }
    color!(text, "text");
    color!(muted, "muted");
    color!(dim, "dim");
    color!(accent, "accent");
    color!(error, "error");
    color!(success, "success");
    color!(warning, "warning");
    color!(info, "info");
    color!(background, "background", "bg");
    color!(surface, "surface", "userMessageBg");
    color!(border, "border");
    color!(border_accent, "borderAccent");
    color!(border_muted, "borderMuted");
    color!(selection, "selection", "selectedBg");
    color!(cursor, "cursor");
    color!(thinking_text, "thinkingText");
    color!(md_heading, "mdHeading");
    color!(md_link, "mdLink");
    color!(md_link_url, "mdLinkUrl");
    color!(md_code, "mdCode");
    color!(md_code_bg, "mdCodeBg");
    color!(md_code_block, "mdCodeBlock");
    color!(md_code_block_bg, "mdCodeBlockBg");
    color!(md_code_block_border, "mdCodeBlockBorder");
    color!(md_quote, "mdQuote");
    color!(md_quote_border, "mdQuoteBorder");
    color!(md_hr, "mdHr");
    color!(md_list_bullet, "mdListBullet");
    color!(tool_pending_bg, "toolPendingBg");
    color!(tool_success_bg, "toolSuccessBg");
    color!(tool_error_bg, "toolErrorBg");
    color!(tool_title, "toolTitle");
    color!(tool_output, "toolOutput");
    color!(bash_mode, "bashMode");
    color!(tool_diff_added, "toolDiffAdded");
    color!(tool_diff_removed, "toolDiffRemoved");
    color!(tool_diff_context, "toolDiffContext");
    if let Some(border) = value.get("borderStyle").and_then(Value::as_str) {
        theme.border_style = match border.to_ascii_lowercase().as_str() {
            "sharp" => rpi_tui::theme::BorderStyle::Sharp,
            "double" => rpi_tui::theme::BorderStyle::Double,
            "thick" => rpi_tui::theme::BorderStyle::Thick,
            "none" => rpi_tui::theme::BorderStyle::None,
            _ => rpi_tui::theme::BorderStyle::Rounded,
        };
    }
    if let Some(corner) = value.get("cornerStyle").and_then(Value::as_str) {
        theme.corner_style = if corner.eq_ignore_ascii_case("sharp") {
            rpi_tui::theme::CornerStyle::Sharp
        } else {
            rpi_tui::theme::CornerStyle::Rounded
        };
    }
    Ok(theme)
}

fn first_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| value.get(*key))
}

fn parse_color(value: &Value) -> Option<rpi_tui::Color> {
    match value {
        Value::String(raw) => {
            let value = raw.trim();
            let hex = value.strip_prefix('#')?;
            if hex.len() == 6 {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(rpi_tui::Color::Rgb(r, g, b))
            } else if let Some(index) = value.strip_prefix("ansi256:") {
                Some(rpi_tui::Color::Ansi256(index.parse().ok()?))
            } else {
                None
            }
        }
        Value::Array(values) if values.len() == 3 => Some(rpi_tui::Color::Rgb(
            values[0].as_u64()?.try_into().ok()?,
            values[1].as_u64()?.try_into().ok()?,
            values[2].as_u64()?.try_into().ok()?,
        )),
        Value::Object(map) => {
            let r = map.get("r")?.as_u64()?.try_into().ok()?;
            let g = map.get("g")?.as_u64()?.try_into().ok()?;
            let b = map.get("b")?.as_u64()?.try_into().ok()?;
            Some(rpi_tui::Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

/// `rpi package ...` command for managing the enabled Pi package list. This is
/// a local package manager; use `install-pi` when the package must be fetched.
pub fn run_cli(args: &[String]) -> i32 {
    crate::args::normalize_offline_mode(args);
    let args = crate::args::without_offline_flag(args);
    let args = args.as_slice();
    let command = args.first().map(String::as_str).unwrap_or("list");
    let cwd = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: could not determine current directory: {error}");
            return 1;
        }
    };
    match command {
        "list" => {
            let project_trusted = match package_command_project_trusted(&cwd, &args[1..]) {
                Ok(trusted) => trusted,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            };
            let resources = if project_trusted {
                discover_from_settings(&cwd)
            } else {
                discover_from_global_settings(&cwd)
            };
            let native = crate::install::installed_native_packages();
            if args.iter().any(|arg| arg == "--json") {
                let mut values: Vec<_> = resources
                    .packages
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "version": p.version,
                            "type": "ts",
                            "root": p.root,
                            "manifest": p.manifest,
                            "skills": p.skill_dirs_for_display(),
                            "prompts": p.prompt_dirs_for_display(),
                            "themes": p.theme_files_for_display(),
                        })
                    })
                    .collect();
                values.extend(native.iter().map(|package| {
                    serde_json::json!({
                        "name": package.name,
                        "version": package.version,
                        "type": "rust",
                        "source": package.source,
                    })
                }));
                println!(
                    "{}",
                    serde_json::to_string_pretty(&values).unwrap_or_else(|_| "[]".into())
                );
            } else if resources.packages.is_empty() && native.is_empty() {
                println!("no Pi packages enabled");
            } else {
                for package in &resources.packages {
                    let version = package.version.as_deref().unwrap_or("-");
                    println!("{}@{} {}", package.name, version, package.root.display());
                }
                for package in native {
                    let source = package.source.as_deref().unwrap_or("crates.io");
                    println!("{}@{} [rust] {}", package.name, package.version, source);
                }
            }
            for diagnostic in resources.diagnostics {
                eprintln!(
                    "warning: package {}: {}",
                    diagnostic.spec, diagnostic.message
                );
            }
            0
        }
        "add" => {
            let Some(spec) = args.get(1).filter(|s| !s.starts_with('-')) else {
                eprintln!("error: missing package path or name");
                print_help();
                return 2;
            };
            if let Err(error) = resolve_package(&cwd, spec) {
                eprintln!("error: {error}");
                return 1;
            }
            let mut settings = match crate::settings::load_settings() {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("error: refusing to change unreadable package settings: {error}");
                    return 1;
                }
            };
            let packages = settings.packages.get_or_insert_with(Vec::new);
            if !packages.iter().any(|existing| existing.source() == spec) {
                packages.push(crate::settings::PackageSetting::from(spec.clone()));
                if let Err(error) = crate::settings::save_settings(&settings) {
                    eprintln!("error: could not save package settings: {error}");
                    return 1;
                }
                println!("enabled Pi package {spec}");
            } else {
                println!("Pi package already enabled: {spec}");
            }
            0
        }
        "remove" | "rm" => {
            let Some(spec) = args.get(1).filter(|s| !s.starts_with('-')) else {
                eprintln!("error: missing package path or name");
                print_help();
                return 2;
            };
            let mut settings = match crate::settings::load_settings() {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("error: refusing to change unreadable package settings: {error}");
                    return 1;
                }
            };
            let Some(packages) = settings.packages.as_mut() else {
                println!("Pi package is not enabled: {spec}");
                return 0;
            };
            let before = packages.len();
            packages.retain(|existing| existing.source() != spec);
            if packages.len() == before {
                println!("Pi package is not enabled: {spec}");
                return 0;
            }
            if packages.is_empty() {
                settings.packages = None;
            }
            if let Err(error) = crate::settings::save_settings(&settings) {
                eprintln!("error: could not save package settings: {error}");
                return 1;
            }
            println!("disabled Pi package {spec}");
            0
        }
        "update" => {
            if args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
            {
                print_update_help();
                return 0;
            }
            let project_trusted = match package_command_project_trusted(&cwd, &args[1..]) {
                Ok(trusted) => trusted,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            };
            update_packages_with_scope(&cwd, project_trusted, UpdateScope::Native)
        }
        "help" | "--help" | "-h" => {
            print_help();
            0
        }
        other => {
            eprintln!("error: unknown package command `{other}`");
            print_help();
            2
        }
    }
}

/// Compatibility helper for callers that need the Rust-native package scope.
pub fn run_native_update(args: &[String]) -> i32 {
    run_top_level_update(args, UpdateScope::Native)
}

/// Top-level `rpi pi-package update`: update only configured Pi npm/Git
/// packages.
pub fn run_pi_package_update(args: &[String]) -> i32 {
    // Consume the explicit `update` subcommand before handling shared flags.
    if args.first().map(String::as_str) == Some("update") {
        run_top_level_update(&args[1..], UpdateScope::Pi)
    } else {
        run_top_level_update(args, UpdateScope::Pi)
    }
}

fn run_top_level_update(args: &[String], scope: UpdateScope) -> i32 {
    crate::args::normalize_offline_mode(args);
    let args = crate::args::without_offline_flag(args);
    let cwd = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: could not determine current directory: {error}");
            return 1;
        }
    };
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        print_scoped_update_help(scope);
        return 0;
    }
    let project_trusted = if scope.includes_pi() {
        match package_command_project_trusted(&cwd, &args) {
            Ok(trusted) => trusted,
            Err(error) => {
                eprintln!("error: {error}");
                return 2;
            }
        }
    } else {
        if let Some(arg) = args.first() {
            eprintln!("error: unknown native update option `{arg}`");
            return 2;
        }
        false
    };
    update_packages_with_scope(&cwd, project_trusted, scope)
}

fn print_help() {
    println!(
        "Usage: rpi package <command>\n\nCommands:\n  list [--json] [--approve|--no-approve]\n                     List enabled TS packages and installed Rust extensions\n  add <path-or-name> Enable a local/package.json package\n  remove <path-or-name>\n                     Disable a Pi package\n  update [--offline]\n                     Update installed Rust-native extensions\n\nProject packages load by default without confirmation; use --no-approve to disable project package access. TS package resources are loaded from skills/, prompts/, themes/, SYSTEM.md, APPEND_SYSTEM.md, and extensions. Rust-native extensions are installed with `rpi install`. Pi packages are updated with `rpi pi-package update`."
    );
}

fn print_update_help() {
    println!(
        "Usage: rpi package update [--offline]\n\nUpdate installed Rust-native extensions only.\n\nUse `rpi pi-package update` for configured Pi npm/Git packages."
    );
}

fn print_scoped_update_help(scope: UpdateScope) {
    match scope {
        UpdateScope::Native => println!(
            "Usage: rpi package update [--offline]\n\nUpdate installed Rust-native extensions only."
        ),
        UpdateScope::Pi => println!(
            "Usage: rpi pi-package update [--approve|--no-approve] [--offline]\n\nUpdate configured Pi npm/Git packages only.\n\nThe rpi CLI itself is updated with `rpi update`."
        ),
    }
}

fn package_command_project_trusted(cwd: &Path, args: &[String]) -> Result<bool, String> {
    let mut override_value = None;
    for arg in args {
        let value = match arg.as_str() {
            "--approve" | "-a" => Some(true),
            "--no-approve" | "-na" => Some(false),
            "--json" => None,
            value => return Err(format!("unknown package option `{value}`")),
        };
        if let Some(value) = value {
            if override_value.replace(value).is_some() {
                return Err("--approve and --no-approve cannot be combined or repeated".to_string());
            }
        }
    }
    if let Some(value) = override_value {
        return Ok(value);
    }
    Ok(crate::config::project_trust_decision(cwd)
        .map_err(|error| error.to_string())?
        .unwrap_or(true))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateScope {
    Native,
    Pi,
}

impl UpdateScope {
    fn includes_native(self) -> bool {
        matches!(self, Self::Native)
    }

    fn includes_pi(self) -> bool {
        matches!(self, Self::Pi)
    }
}

fn update_packages_with_scope(cwd: &Path, project_trusted: bool, scope: UpdateScope) -> i32 {
    if crate::args::offline_env_enabled() {
        println!("package update skipped: offline mode is enabled");
        return 0;
    }
    // Native updates validate their registry before mutating anything. Pi
    // package updates independently validate settings before recovering an
    // interrupted npm/Git directory swap.
    let native = if scope.includes_native() {
        match crate::install::installed_native_packages_strict() {
            Ok(packages) => packages,
            Err(error) => {
                eprintln!(
                    "error: refusing native package update while metadata is invalid: {error}"
                );
                return 1;
            }
        }
    } else {
        Vec::new()
    };
    // Load every settings document before performing Pi package recovery or
    // invoking a package manager. A malformed active file must make the Pi
    // update a no-op rather than silently narrowing the requested package set.
    let (resources, preflight_npm_command) = if scope.includes_pi() {
        match discover_from_settings_for_update(cwd, project_trusted) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("error: refusing Pi package update with unreadable settings: {error}");
                return 1;
            }
        }
    } else {
        (PackageResources::default(), None)
    };
    let blocked = resources
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.blocks_update)
        .count();
    for diagnostic in &resources.diagnostics {
        eprintln!(
            "warning: could not load package {}: {}",
            diagnostic.spec, diagnostic.message
        );
    }
    if blocked > 0 {
        eprintln!("error: refusing a partial package update after discovery failures");
        return 1;
    }
    let needs_package_command = resources.packages.iter().any(|package| {
        package.updateable_npm_source().is_some() || package.updateable_git_source()
    });
    let npm_command = if needs_package_command {
        match preflight_npm_command {
            Some(command) => Some(command),
            None => {
                eprintln!("error: package update command was not validated before discovery");
                return 1;
            }
        }
    } else {
        None
    };
    let mut failed = 0;
    if resources.packages.is_empty() && native.is_empty() {
        println!("no packages available for update");
        return 0;
    }
    let mut updated = 0;
    let mut skipped = 0;
    for package in native {
        if package.source.is_some() {
            println!(
                "skipped local Rust package {} (no registry source)",
                package.name
            );
            skipped += 1;
            continue;
        }
        let args = vec![package.name.clone(), "--force".to_string()];
        if crate::install::run(&args) == 0 {
            updated += 1;
        } else {
            eprintln!("warning: could not update Rust package {}", package.name);
            failed += 1;
        }
    }

    let mut standalone_npm = Vec::new();
    let mut npm_store_roots: BTreeMap<PathBuf, Vec<(String, String)>> = BTreeMap::new();
    let mut git_updates = Vec::new();
    for package in resources.packages {
        if let Some((name, source_spec)) = package.updateable_npm_source() {
            match package.npm_store_root_for_update(cwd, project_trusted) {
                Ok(Some(install_root)) => {
                    npm_store_roots
                        .entry(install_root)
                        .or_default()
                        .push((name.to_string(), source_spec.to_string()));
                }
                Ok(None) => {
                    standalone_npm.push((
                        package.root.clone(),
                        package.name.clone(),
                        name.to_string(),
                        source_spec.to_string(),
                    ));
                }
                Err(error) => {
                    eprintln!(
                        "warning: could not plan update for {}: {error}",
                        package.name
                    );
                    failed += 1;
                }
            }
        } else if package.updateable_git_source() {
            git_updates.push(package);
        } else {
            println!(
                "skipped package {} (not an unpinned npm source)",
                package.name
            );
            skipped += 1;
        }
    }

    let npm_update_count = standalone_npm.len()
        + npm_store_roots
            .values()
            .map(std::vec::Vec::len)
            .sum::<usize>();
    let git_update_count = git_updates.len();
    if npm_update_count > 0 || git_update_count > 0 {
        match npm_command.as_ref() {
            Some(npm_command) => {
                for (root, display_name, name, source_spec) in standalone_npm {
                    match crate::install_pi::update_npm_package(
                        &root,
                        &name,
                        &source_spec,
                        &npm_command,
                    ) {
                        Ok(_) => {
                            println!("updated npm package {display_name}");
                            updated += 1;
                        }
                        Err(error) => {
                            eprintln!("warning: could not update {display_name}: {error}");
                            failed += 1;
                        }
                    }
                }
                for (root, packages) in npm_store_roots {
                    match crate::install_pi::update_npm_store_root(
                        &root,
                        &packages,
                        &npm_command,
                        cwd,
                        project_trusted,
                    ) {
                        Ok(()) => {
                            for (name, _) in &packages {
                                println!("updated npm package {name}");
                            }
                            updated += packages.len();
                        }
                        Err(error) => {
                            let names = packages
                                .iter()
                                .map(|(name, _)| name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            eprintln!(
                                "warning: could not update npm packages {names} in {}: {error}",
                                root.display()
                            );
                            failed += packages.len();
                        }
                    }
                }
                for package in git_updates {
                    if package.missing_install {
                        match crate::install_pi::install_missing_git_package(
                            cwd,
                            package.scope == ResolveScope::User,
                            &package.spec,
                            &npm_command,
                        ) {
                            Ok(_) => {
                                println!("updated git package {}", package.name);
                                updated += 1;
                            }
                            Err(error) => {
                                eprintln!("warning: could not update {}: {error}", package.name);
                                failed += 1;
                            }
                        }
                        continue;
                    }
                    let Some(store_root) = package.safe_git_store_root(cwd) else {
                        eprintln!(
                            "warning: refusing to update git package {} outside a managed git store",
                            package.name
                        );
                        failed += 1;
                        continue;
                    };
                    match crate::install_pi::update_git_package(
                        &package.root,
                        &store_root,
                        &package.spec,
                        &npm_command,
                    ) {
                        Ok(()) => {
                            println!("updated git package {}", package.name);
                            updated += 1;
                        }
                        Err(error) => {
                            eprintln!("warning: could not update {}: {error}", package.name);
                            failed += 1;
                        }
                    }
                }
            }
            None => unreachable!("package command was preflighted for update candidates"),
        }
    }
    let label = match scope {
        UpdateScope::Native => "native package update",
        UpdateScope::Pi => "Pi package update",
    };
    println!("{label} complete: {updated} updated, {skipped} skipped");
    i32::from(failed > 0)
}

fn is_npm_store_package_path(path: &Path, cwd: &Path, scope: ResolveScope) -> bool {
    npm_install_root_for_path(path, cwd, scope).is_some()
}

fn npm_install_root_for_path(path: &Path, cwd: &Path, scope: ResolveScope) -> Option<PathBuf> {
    if matches!(scope, ResolveScope::Any | ResolveScope::Project) {
        if let Some(root) = package_manager_root_for_path(path, cwd, Path::new(".pi/npm")) {
            return Some(root);
        }
    }
    if matches!(scope, ResolveScope::Any | ResolveScope::User) {
        if let Ok(agent) = config::agent_dir() {
            if let Some(root) = package_manager_root_for_path(path, &agent, Path::new("npm")) {
                return Some(root);
            }
        }
        if let Some(home) = dirs::home_dir() {
            if let Some(root) =
                package_manager_root_for_path(path, &home, Path::new(".pi/agent/npm"))
            {
                return Some(root);
            }
        }
    }
    None
}

fn git_store_roots(cwd: &Path, scope: ResolveScope) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if matches!(scope, ResolveScope::Any | ResolveScope::Project) {
        roots.push(cwd.join(".rpi/git"));
        roots.push(cwd.join(".pi/git"));
    }
    if matches!(scope, ResolveScope::Any | ResolveScope::User) {
        if let Ok(agent) = config::agent_dir() {
            roots.push(agent.join("git"));
        }
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".pi/agent/git"));
        }
    }
    roots
}

/// Return the native Pi git checkout for a URL only when both the store and
/// the checkout are real directories (no symlink/junction traversal) and the
/// relative host/path matches exactly. This keeps `git pull` authority inside
/// the configured store.
fn native_git_target_for_spec(cwd: &Path, scope: ResolveScope, git: &GitSpec) -> Option<PathBuf> {
    let relative = Path::new(&git.host).join(&git.path);
    for lexical_root in git_store_roots(cwd, scope) {
        let Ok(canonical_root_raw) = std::fs::canonicalize(&lexical_root) else {
            continue;
        };
        let canonical_root = normalize_resource_path(canonical_root_raw);
        if canonical_root != lexical_root {
            continue;
        }
        let target = lexical_root.join(&relative);
        let Ok(canonical_target_raw) = std::fs::canonicalize(&target) else {
            continue;
        };
        let canonical_target = normalize_resource_path(canonical_target_raw);
        if canonical_target == target
            && canonical_target.starts_with(&canonical_root)
            && canonical_target
                .strip_prefix(&canonical_root)
                .ok()
                .is_some_and(|value| value.components().count() == relative.components().count())
            && is_real_git_metadata(&canonical_target.join(".git"))
        {
            return Some(canonical_target);
        }
    }
    None
}

fn is_native_git_package_path(path: &Path, cwd: &Path, scope: ResolveScope) -> bool {
    let Ok(canonical_path) = std::fs::canonicalize(path) else {
        return false;
    };
    let canonical_path = normalize_resource_path(canonical_path);
    if canonical_path != path || !is_real_git_metadata(&canonical_path.join(".git")) {
        return false;
    }
    git_store_roots(cwd, scope).into_iter().any(|root| {
        let Ok(canonical_root_raw) = std::fs::canonicalize(&root) else {
            return false;
        };
        let canonical_root = normalize_resource_path(canonical_root_raw);
        canonical_root == root
            && canonical_path
                .strip_prefix(&canonical_root)
                .ok()
                .is_some_and(|relative| relative.components().count() >= 2)
    })
}

fn native_git_store_root_for_path(path: &Path, cwd: &Path, scope: ResolveScope) -> Option<PathBuf> {
    let Ok(canonical_path_raw) = std::fs::canonicalize(path) else {
        return None;
    };
    let canonical_path = normalize_resource_path(canonical_path_raw);
    if canonical_path != path || !is_real_git_metadata(&canonical_path.join(".git")) {
        return None;
    }
    git_store_roots(cwd, scope).into_iter().find_map(|root| {
        let canonical_root = normalize_resource_path(std::fs::canonicalize(&root).ok()?);
        if canonical_root != root {
            return None;
        }
        let relative = canonical_path.strip_prefix(&canonical_root).ok()?;
        (relative.components().count() >= 2).then_some(canonical_root)
    })
}

fn is_direct_managed_package_root(path: &Path, cwd: &Path, scope: ResolveScope) -> Option<PathBuf> {
    let canonical_path = normalize_resource_path(std::fs::canonicalize(path).ok()?);
    if canonical_path != path || !is_real_git_metadata(&canonical_path.join(".git")) {
        return None;
    }
    let mut stores = Vec::new();
    if let Ok(agent) = config::agent_dir() {
        stores.push(agent.join("packages"));
    }
    if let Some(home) = dirs::home_dir() {
        stores.push(home.join(".pi/agent/packages"));
    }
    if matches!(scope, ResolveScope::Any | ResolveScope::Project) {
        stores.push(cwd.join(".rpi/packages"));
        stores.push(cwd.join(".pi/packages"));
    }
    let parent = canonical_path.parent()?;
    stores
        .iter()
        .find(|store| {
            std::fs::canonicalize(store)
                .ok()
                .map(normalize_resource_path)
                .is_some_and(|canonical| canonical == **store)
                && parent == store.as_path()
        })
        .cloned()
}

fn is_real_git_metadata(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        // A git worktree stores `.git` as a file containing a `gitdir:` pointer.
        // Treating that pointer as package metadata could make the update
        // command operate on a repository outside the managed store. Only a
        // real directory is therefore eligible for automatic updates.
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

/// Recognize exactly one npm package below a Pi-managed install root. Lexical
/// shape prevents nested dependencies from gaining update authority, while
/// the canonical containment check permits pnpm links only when they resolve
/// back inside the same install root.
fn package_manager_root_for_path(
    path: &Path,
    base: &Path,
    relative_install_root: &Path,
) -> Option<PathBuf> {
    let lexical_install_root = base.join(relative_install_root);
    let lexical_node_modules = lexical_install_root.join("node_modules");
    let relative = path.strip_prefix(&lexical_node_modules).ok()?;
    let parts = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let valid_shape = match parts.as_slice() {
        [name] => !name.starts_with('@') && !name.is_empty(),
        [scope, name] => scope.starts_with('@') && scope.len() > 1 && !name.is_empty(),
        _ => false,
    };
    if !valid_shape {
        return None;
    }

    let base = std::fs::canonicalize(base).ok()?;
    let install_root = base.join(relative_install_root);
    if normalize_resource_path(std::fs::canonicalize(&install_root).ok()?)
        != normalize_resource_path(install_root.clone())
    {
        return None;
    }
    let node_modules = install_root.join("node_modules");
    if normalize_resource_path(std::fs::canonicalize(&node_modules).ok()?)
        != normalize_resource_path(node_modules.clone())
    {
        return None;
    }
    let canonical_package = normalize_resource_path(std::fs::canonicalize(path).ok()?);
    let install_root_normalized = normalize_resource_path(install_root.clone());
    canonical_package
        .starts_with(&install_root_normalized)
        .then_some(install_root)
}

fn is_package_below_store(path: &Path, base: &Path, relative_store: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(base) = std::fs::canonicalize(base) else {
        return false;
    };
    let Ok(store) = std::fs::canonicalize(base.join(relative_store)) else {
        return false;
    };
    if store != base.join(relative_store) || !store.starts_with(&base) {
        return false;
    }
    path.strip_prefix(store)
        .ok()
        .is_some_and(|relative| relative.components().next().is_some())
}

fn is_managed_package_path(path: &Path, cwd: &Path, scope: ResolveScope) -> bool {
    if let Ok(agent) = config::agent_dir() {
        if is_package_below_store(path, &agent, Path::new("packages")) {
            return true;
        }
    }
    if let Some(home) = dirs::home_dir() {
        if is_package_below_store(path, &home, Path::new(".pi/agent/packages")) {
            return true;
        }
    }
    (scope == ResolveScope::Any
        && (is_package_below_store(path, cwd, Path::new(".rpi/packages"))
            || is_package_below_store(path, cwd, Path::new(".pi/packages"))))
        || is_project_managed_package_path(path)
}

/// Installed project packages are persisted as absolute `file:` entries in
/// user settings, so they must remain recognizable after the process changes
/// working directory. Canonicalizing first prevents a symlink placed at this
/// shape from granting update permission to an arbitrary target directory.
fn is_project_managed_package_path(path: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let Some(store) = path.parent() else {
        return false;
    };
    let Some(project_config) = store.parent() else {
        return false;
    };
    store.file_name().is_some_and(|name| name == "packages")
        && project_config
            .file_name()
            .is_some_and(|name| name == ".rpi" || name == ".pi")
}

impl PackageRoot {
    fn npm_store_root_for_update(
        &self,
        cwd: &Path,
        project_trusted: bool,
    ) -> Result<Option<PathBuf>, String> {
        if let Some(root) = &self.npm_install_root {
            return Ok(Some(root.clone()));
        }
        if self.legacy_npm_root.is_none() {
            return Ok(None);
        }
        if !matches!(self.source, PackageSource::Npm { .. }) {
            return Err(
                "refusing legacy npm migration without verified npm provenance".to_string(),
            );
        }

        let root = match self.scope {
            ResolveScope::Project if project_trusted => cwd.join(".pi/npm"),
            ResolveScope::Project => {
                return Err(
                    "refusing to migrate a legacy npm package for an untrusted project".to_string(),
                )
            }
            ResolveScope::User | ResolveScope::Any => config::agent_dir()
                .map_err(|error| error.to_string())?
                .join("npm"),
        };
        if !root.is_absolute() || root.file_name().and_then(|name| name.to_str()) != Some("npm") {
            return Err(format!(
                "refusing legacy npm migration outside a managed npm root: {}",
                root.display()
            ));
        }
        Ok(Some(root))
    }

    pub(crate) fn updateable_npm_source(&self) -> Option<(&str, &str)> {
        match &self.source {
            PackageSource::Npm {
                name, spec, pinned, ..
            } if self.missing_install || !*pinned => Some((name, spec)),
            _ => None,
        }
    }

    fn updateable_git_source(&self) -> bool {
        // Native Pi treats a Git ref as a configured checkout target. Manual
        // update reconciles it as well; only automatic update notifications
        // skip pinned sources.
        matches!(self.source, PackageSource::Git)
    }

    fn safe_git_store_root(&self, cwd: &Path) -> Option<PathBuf> {
        self.git_store_root.clone().or_else(|| {
            // rpi's legacy git clones live as direct children of a managed
            // package store. Native stores carry an explicit root above.
            is_direct_managed_package_root(&self.root, cwd, self.scope)
        })
    }

    #[cfg(test)]
    pub(crate) fn updateable_npm_name(&self) -> Option<&str> {
        self.updateable_npm_source().map(|(name, _)| name)
    }

    fn skill_dirs_for_display(&self) -> Vec<PathBuf> {
        self.skills.clone()
    }

    fn prompt_dirs_for_display(&self) -> Vec<PathBuf> {
        self.prompts.clone()
    }

    fn theme_files_for_display(&self) -> Vec<PathBuf> {
        if self.themes.len() == 1 && self.themes[0].is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&self.themes[0])
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|file| {
                    file.is_file() && file.extension().and_then(|ext| ext.to_str()) == Some("json")
                })
                .collect();
            files.sort();
            files
        } else {
            self.themes.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolveScope {
    Any,
    Project,
    User,
}

#[derive(Debug)]
struct ResolvedPackagePath {
    root: PathBuf,
    /// Set only when the path came from a validated package-manager global
    /// lookup. Static rpi/Pi stores leave this unset.
    legacy_npm_root: Option<PathBuf>,
}

fn resolve_spec(cwd: &Path, spec: &str, scope: ResolveScope) -> Option<PathBuf> {
    resolve_spec_with_legacy_lookup(cwd, spec, scope, |_| None).map(|resolved| resolved.root)
}

fn resolve_spec_with_command(
    cwd: &Path,
    spec: &str,
    scope: ResolveScope,
    global_npm_command: Option<&crate::npm::NpmCommand>,
    legacy_npm_names: &[String],
    legacy_npm_paths: &mut Option<HashMap<String, PathBuf>>,
) -> Option<ResolvedPackagePath> {
    resolve_spec_with_legacy_lookup(cwd, spec, scope, |package_name| {
        let command = global_npm_command?;
        let paths = legacy_npm_paths.get_or_insert_with(|| {
            command
                .global_package_paths(legacy_npm_names)
                .unwrap_or_default()
        });
        paths.get(package_name).cloned()
    })
}

fn resolve_spec_with_legacy_lookup(
    cwd: &Path,
    spec: &str,
    scope: ResolveScope,
    legacy_lookup: impl FnOnce(&str) -> Option<PathBuf>,
) -> Option<ResolvedPackagePath> {
    let file_spec = spec.strip_prefix("file:");
    let raw = file_spec.unwrap_or(spec);
    // `npm:` is a package-source prefix, not part of the on-disk package
    // name. Keeping it in the candidates makes installed npm packages look
    // like directories literally named `npm:...`.
    let npm_spec = raw.strip_prefix("npm:");
    let npm_name = npm_spec.unwrap_or(raw);
    let package_name = match npm_spec {
        Some(spec) => parse_npm_package_spec(spec)?.install_name,
        None => package_name_without_version(npm_name).to_string(),
    };
    let package_key = package_name
        .strip_prefix('@')
        .unwrap_or(&package_name)
        .replace('/', "__");
    let direct = PathBuf::from(npm_name);
    let mut candidates = Vec::new();
    if direct.is_absolute() {
        candidates.push(direct);
    } else {
        let explicit_relative_path =
            file_spec.is_some() || npm_name.starts_with('.') || npm_name.starts_with("./");
        if explicit_relative_path {
            if matches!(scope, ResolveScope::Any | ResolveScope::Project) {
                // Native Pi resolves project-local package paths from the
                // project config directory (`.pi`); rpi's preferred `.rpi`
                // directory is accepted first for its own settings.
                candidates.push(cwd.join(".rpi").join(&direct));
                candidates.push(cwd.join(".pi").join(&direct));
                // Keep the historical cwd-relative fallback for callers of
                // the public `discover` helper and old rpi settings.
                candidates.push(cwd.join(&direct));
            } else {
                if let Ok(agent) = config::agent_dir() {
                    candidates.push(agent.join(&direct));
                }
                if let Some(home) = dirs::home_dir() {
                    candidates.push(home.join(".pi/agent").join(&direct));
                }
            }
        }
        if let Some(git) = parse_git_source(spec) {
            if matches!(scope, ResolveScope::Any | ResolveScope::Project) {
                for relative_root in [Path::new(".rpi/git"), Path::new(".pi/git")] {
                    candidates.push(cwd.join(relative_root).join(&git.host).join(&git.path));
                }
            }
            if matches!(scope, ResolveScope::Any | ResolveScope::User) {
                if let Ok(agent) = config::agent_dir() {
                    candidates.push(agent.join("git").join(&git.host).join(&git.path));
                }
                if let Some(home) = dirs::home_dir() {
                    candidates.push(home.join(".pi/agent/git").join(&git.host).join(&git.path));
                }
            }
        }
        // Prefer rpi-owned package stores over native Pi stores and generic
        // node_modules when a bare package name resolves in more than one
        // place.
        if matches!(scope, ResolveScope::Any | ResolveScope::Project) {
            candidates.push(cwd.join(".rpi/packages").join(&package_name));
            if package_key != package_name {
                candidates.push(cwd.join(".rpi/packages").join(&package_key));
            }
            candidates.push(cwd.join(".pi/packages").join(&package_name));
            if package_key != package_name {
                candidates.push(cwd.join(".pi/packages").join(&package_key));
            }
            if npm_spec.is_some() {
                candidates.push(cwd.join(".pi/npm/node_modules").join(&package_name));
            } else if scope == ResolveScope::Any {
                for ancestor in cwd.ancestors() {
                    candidates.push(ancestor.join("node_modules").join(&package_name));
                }
            }
        }
        if matches!(scope, ResolveScope::Any | ResolveScope::User) {
            if let Ok(agent) = config::agent_dir() {
                candidates.push(agent.join("packages").join(&package_name));
                if package_key != package_name {
                    candidates.push(agent.join("packages").join(&package_key));
                }
                // Pi's native npm installer keeps packages under
                // ~/.pi/agent/npm/node_modules rather than ~/.pi/agent/packages.
                // Keep the same layout usable when rpi reads Pi's settings.json.
                candidates.push(agent.join("npm/node_modules").join(&package_name));
                if package_key != package_name {
                    candidates.push(agent.join("npm/node_modules").join(&package_key));
                }
            }
            if let Some(home) = dirs::home_dir() {
                // Keep native Pi's installed package store usable when the user
                // has not copied it into the rpi-owned config directory yet.
                candidates.push(home.join(".pi/agent/packages").join(&package_name));
                if package_key != package_name {
                    candidates.push(home.join(".pi/agent/packages").join(&package_key));
                }
                candidates.push(home.join(".pi/agent/npm/node_modules").join(&package_name));
                if package_key != package_name {
                    candidates.push(home.join(".pi/agent/npm/node_modules").join(&package_key));
                }
            }
        }
        if scope == ResolveScope::Any && npm_spec.is_none() && !explicit_relative_path {
            candidates.push(cwd.join(&package_name));
        }
    }
    for candidate in candidates {
        if candidate.is_file()
            && candidate.file_name().and_then(|s| s.to_str()) == Some("package.json")
        {
            return candidate.parent().map(|root| ResolvedPackagePath {
                root: root.to_path_buf(),
                legacy_npm_root: None,
            });
        }
        if candidate.is_dir() {
            return Some(ResolvedPackagePath {
                root: candidate,
                legacy_npm_root: None,
            });
        }
    }

    // Native Pi can still load a package installed by the user's global
    // package manager. This is a read-only compatibility lookup: the command
    // validates and canonicalizes the package path, while update code later
    // migrates it into the controlled rpi/native npm store.
    if npm_spec.is_some() && matches!(scope, ResolveScope::Any | ResolveScope::User) {
        let reported = legacy_lookup(&package_name)?;
        let reported = std::fs::canonicalize(reported).ok()?;
        let install_root = global_node_modules_root(&reported)?;
        // Repeat the direct-child/canonical containment check at the package
        // boundary. Even a future lookup implementation cannot turn an
        // arbitrary command output path into update/delete authority.
        let root =
            crate::npm::NpmCommand::validate_global_package_path(&install_root, &package_name)?;
        if root != reported {
            return None;
        }
        return Some(ResolvedPackagePath {
            root,
            legacy_npm_root: Some(install_root),
        });
    }
    None
}

fn global_node_modules_root(package: &Path) -> Option<PathBuf> {
    let mut current = package.parent()?;
    loop {
        if current.file_name().and_then(|name| name.to_str()) == Some("node_modules") {
            return std::fs::canonicalize(current).ok().filter(|root| {
                root.is_absolute()
                    && root.file_name().and_then(|name| name.to_str()) == Some("node_modules")
            });
        }
        current = current.parent()?;
    }
}

/// Strip an npm version suffix while preserving the `@scope/name` portion.
fn package_name_without_version(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix('@') {
        rest.find('@')
            .map(|index| &name[..index + 1])
            .unwrap_or(name)
    } else {
        name.split('@').next().unwrap_or(name)
    }
}

/// Build the same collision identity native Pi uses: npm package names ignore
/// the requested range/tag, git packages use their normalized repository
/// identity, and local packages use their canonical path. Manifest names are
/// deliberately not used because two independent packages may publish the
/// same display name.
fn package_identity(package: &PackageRoot) -> String {
    match &package.source {
        PackageSource::Npm { name, .. } => format!("npm:{}", name.to_ascii_lowercase()),
        PackageSource::Git => parse_git_source(&package.spec)
            .map(|git| format!("git:{}/{}", git.host, git.path))
            .unwrap_or_else(|| format!("git:path:{}", normalize_key(&package.root))),
        PackageSource::Local => format!("local:{}", normalize_key(&package.root)),
        PackageSource::Unknown => format!("unknown:{}", normalize_key(&package.root)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitSpec {
    pub(crate) host: String,
    pub(crate) path: String,
    pub(crate) revision: Option<String>,
    pub(crate) transport: GitTransport,
    pub(crate) port: Option<u16>,
    pub(crate) user_info: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitTransport {
    Http,
    Https,
    Ssh,
    Git,
}

impl GitTransport {
    pub(crate) fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
            Self::Ssh => 22,
            Self::Git => 9418,
        }
    }
}

pub(crate) fn parse_git_source(spec: &str) -> Option<GitSpec> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return None;
    }

    // `git:` is Pi's source prefix, while `git://` is also a valid transport
    // URL. Do not strip the latter's scheme accidentally.
    let raw = match trimmed.strip_prefix("git:") {
        Some(rest) if !rest.starts_with("//") => rest.trim(),
        _ => trimmed,
    };
    if raw.is_empty() {
        return None;
    }

    // Native Pi splits the first `@` in the repository path, not the last
    // one. This preserves refs such as `feature/branch` and avoids treating
    // URL user-info (`git@host`) as a ref.
    let (repo, revision) = split_git_ref(raw);
    let (mut host, mut path, transport, port, user_info) =
        if let Some(scheme_end) = repo.find("://") {
            let scheme = repo[..scheme_end].to_ascii_lowercase();
            let transport = match scheme.as_str() {
                "http" => GitTransport::Http,
                "https" => GitTransport::Https,
                "ssh" => GitTransport::Ssh,
                "git" => GitTransport::Git,
                _ => return None,
            };
            let authority_and_path = &repo[scheme_end + 3..];
            let (authority, path) = authority_and_path.split_once('/')?;
            let (host, port, user_info) = parse_git_authority(authority)?;
            (host, path.to_string(), transport, port, user_info)
        } else if let Some(rest) = repo.strip_prefix("git@") {
            let (host, path) = rest.split_once(':')?;
            (
                normalize_git_host(host)?,
                path.to_string(),
                GitTransport::Ssh,
                None,
                Some("git".to_string()),
            )
        } else {
            // Historical `git:github.com/user/repo` shorthand.
            let (host, path) = repo.split_once('/')?;
            (
                normalize_git_host(host)?,
                path.to_string(),
                GitTransport::Https,
                None,
                None,
            )
        };

    host.make_ascii_lowercase();
    while path.starts_with('/') {
        path.remove(0);
    }
    if path.ends_with(".git") {
        path.truncate(path.len() - 4);
    }
    let path = path.trim_matches('/').to_string();

    if !safe_git_install_part(&host, false)
        || !safe_git_install_part(&path, true)
        || path.split('/').count() < 2
    {
        return None;
    }
    if revision
        .as_deref()
        .is_some_and(|value| !safe_git_revision(value))
    {
        return None;
    }

    Some(GitSpec {
        host,
        path,
        revision,
        transport,
        port,
        user_info,
    })
}

/// Split a git URL into its repository and optional ref. The separator is
/// searched only after the URL authority, matching the upstream Pi parser.
fn split_git_ref(raw: &str) -> (String, Option<String>) {
    let path_start = if raw.starts_with("git@") {
        raw.find(':').map(|index| index + 1)
    } else if let Some(scheme_end) = raw.find("://") {
        let authority_start = scheme_end + 3;
        raw[authority_start..]
            .find('/')
            .map(|index| authority_start + index + 1)
    } else {
        raw.find('/').map(|index| index + 1)
    };
    let Some(path_start) = path_start else {
        return (raw.to_string(), None);
    };
    let Some(offset) = raw[path_start..].find('@') else {
        return (raw.to_string(), None);
    };
    let separator = path_start + offset;
    let repo = &raw[..separator];
    let revision = &raw[separator + 1..];
    if repo.is_empty() || revision.is_empty() {
        return (raw.to_string(), None);
    }
    (repo.to_string(), Some(revision.to_string()))
}

fn normalize_git_host(authority: &str) -> Option<String> {
    if authority != authority.trim() {
        return None;
    }
    let authority = authority.trim();
    if authority.is_empty() {
        return None;
    }
    // URL.hostname excludes user-info and a numeric port. Keep the same
    // identity semantics while rejecting ambiguous/malformed authorities.
    let host = if authority.starts_with('[') {
        let end = authority.find(']')?;
        if !authority[end + 1..].is_empty() {
            let suffix = &authority[end + 1..];
            if !suffix.starts_with(':') || !suffix[1..].bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
        }
        &authority[1..end]
    } else {
        authority
            .rsplit_once(':')
            .filter(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
            .map_or(authority, |(host, _)| host)
    };
    Some(host.to_string())
}

fn parse_git_authority(authority: &str) -> Option<(String, Option<u16>, Option<String>)> {
    if authority.is_empty() || authority != authority.trim() {
        return None;
    }
    let authority = authority.trim();
    let (user_info, host_and_port) = match authority.rsplit_once('@') {
        Some((user_info, host_and_port)) => {
            if user_info.is_empty()
                || user_info.contains('\\')
                || user_info
                    .chars()
                    .any(|character| character.is_control() || character.is_whitespace())
            {
                return None;
            }
            (Some(user_info.to_string()), host_and_port)
        }
        None => (None, authority),
    };
    let (host, port) = if host_and_port.starts_with('[') {
        let end = host_and_port.find(']')?;
        let suffix = &host_and_port[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            suffix.strip_prefix(':')?.parse::<u16>().ok()
        };
        (&host_and_port[..=end], port)
    } else if let Some((host, port)) = host_and_port.rsplit_once(':') {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        (host, Some(port.parse::<u16>().ok()?))
    } else {
        (host_and_port, None)
    };
    Some((normalize_git_host(host)?, port, user_info))
}

fn safe_git_install_part(value: &str, allow_slash: bool) -> bool {
    let Some(decoded) = percent_decode_for_validation(value) else {
        return false;
    };
    for candidate in [value, decoded.as_str()] {
        if candidate.is_empty()
            || candidate.contains('\0')
            || candidate.contains('\\')
            || candidate.starts_with('/')
            || candidate
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
            || candidate
                .chars()
                .any(|ch| matches!(ch, ':' | '?' | '*' | '[' | ']' | '<' | '>' | '|' | '"'))
        {
            return false;
        }
        if !allow_slash && candidate.contains('/') {
            return false;
        }
        if candidate
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return false;
        }
    }
    true
}

fn safe_git_revision(value: &str) -> bool {
    let Some(decoded) = percent_decode_for_validation(value) else {
        return false;
    };
    for candidate in [value, decoded.as_str()] {
        if candidate.is_empty()
            || candidate.starts_with('-')
            || candidate.starts_with('/')
            || candidate.ends_with('/')
            || candidate.contains('\0')
            || candidate.contains('\\')
            || candidate.contains("..")
            || candidate.contains("@{")
            || candidate.chars().any(|ch| {
                ch.is_control()
                    || ch.is_whitespace()
                    || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[')
            })
            || candidate
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return false;
        }
    }
    true
}

fn percent_decode_for_validation(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn safe_resource_path(root: &Path, value: &str) -> Option<PathBuf> {
    safe_resource_path_from(root, root, value)
}

fn safe_resource_path_from(boundary: &Path, base: &Path, value: &str) -> Option<PathBuf> {
    let relative = Path::new(value.trim());
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return None;
    }
    let base = normalize_resource_path(std::fs::canonicalize(base).ok()?);
    let candidate = normalize_resource_path(base.join(relative));
    validated_resource_path(boundary, &candidate).map(|_| candidate)
}

fn validated_resource_path(boundary: &Path, candidate: &Path) -> Option<PathBuf> {
    let boundary = normalize_resource_path(std::fs::canonicalize(boundary).ok()?);
    let canonical = normalize_resource_path(std::fs::canonicalize(candidate).ok()?);
    resource_path_is_within(&canonical, &boundary).then_some(canonical)
}

fn resource_path_is_within(path: &Path, root: &Path) -> bool {
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
    #[cfg(windows)]
    {
        let path: Vec<String> = path
            .components()
            .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        let root: Vec<String> = root
            .components()
            .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        path.len() >= root.len() && path[..root.len()] == root
    }
}

fn normalize_resource_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{stripped}"));
        }
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterResourceKind {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

fn apply_package_filter(
    root: &Path,
    extensions: &mut Vec<PathBuf>,
    skills: &mut Vec<PathBuf>,
    prompts: &mut Vec<PathBuf>,
    themes: &mut Vec<PathBuf>,
    filter: &crate::settings::PackageFilter,
) {
    *extensions = filter_paths(
        root,
        extensions,
        filter.extensions.as_deref(),
        filter.autoload,
        FilterResourceKind::Extensions,
    );
    *skills = filter_paths(
        root,
        skills,
        filter.skills.as_deref(),
        filter.autoload,
        FilterResourceKind::Skills,
    );
    *prompts = filter_paths(
        root,
        prompts,
        filter.prompts.as_deref(),
        filter.autoload,
        FilterResourceKind::Prompts,
    );
    *themes = filter_paths(
        root,
        themes,
        filter.themes.as_deref(),
        filter.autoload,
        FilterResourceKind::Themes,
    );
}

fn filter_paths(
    root: &Path,
    defaults: &[PathBuf],
    patterns: Option<&[String]>,
    autoload: Option<bool>,
    kind: FilterResourceKind,
) -> Vec<PathBuf> {
    let Some(patterns) = patterns else {
        return if autoload == Some(false) {
            Vec::new()
        } else {
            defaults.to_vec()
        };
    };
    if patterns.is_empty() && autoload != Some(false) {
        // An explicitly empty resource array disables that resource kind in
        // native Pi; it is different from an omitted property.
        return Vec::new();
    }
    let pattern_root =
        normalize_resource_path(std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()));
    let all = resource_inventory(&pattern_root, defaults, kind);
    if autoload == Some(false) {
        let mut enabled = HashSet::new();
        for pattern in patterns {
            let (mode, target) = pattern_mode(pattern);
            let exact = matches!(mode, PatternMode::ForceInclude | PatternMode::ForceExclude);
            for path in &all {
                if matches_resource_pattern(path, &pattern_root, target, exact, kind) {
                    match mode {
                        PatternMode::Exclude | PatternMode::ForceExclude => {
                            enabled.remove(path);
                        }
                        PatternMode::Include | PatternMode::ForceInclude => {
                            enabled.insert(path.clone());
                        }
                    }
                }
            }
        }
        return sorted_paths(enabled.into_iter().collect());
    }
    apply_resource_patterns(&all, patterns, &pattern_root, kind)
}

fn apply_autoload_delta_to_package(
    package: &mut PackageRoot,
    filter: &crate::settings::PackageFilter,
) {
    if filter.autoload != Some(false) {
        return;
    }
    if let Some(patterns) = filter.extensions.as_deref() {
        package.extensions = apply_delta_paths(
            &package.root,
            &package.extensions,
            patterns,
            FilterResourceKind::Extensions,
        );
    }
    if let Some(patterns) = filter.skills.as_deref() {
        package.skills = apply_delta_paths(
            &package.root,
            &package.skills,
            patterns,
            FilterResourceKind::Skills,
        );
    }
    if let Some(patterns) = filter.prompts.as_deref() {
        package.prompts = apply_delta_paths(
            &package.root,
            &package.prompts,
            patterns,
            FilterResourceKind::Prompts,
        );
    }
    if let Some(patterns) = filter.themes.as_deref() {
        package.themes = apply_delta_paths(
            &package.root,
            &package.themes,
            patterns,
            FilterResourceKind::Themes,
        );
    }
}

fn apply_delta_paths(
    root: &Path,
    current: &[PathBuf],
    patterns: &[String],
    kind: FilterResourceKind,
) -> Vec<PathBuf> {
    if patterns.is_empty() {
        return current.to_vec();
    }
    let pattern_root =
        normalize_resource_path(std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()));
    let all = resource_inventory(&pattern_root, current, kind);
    let mut selected: HashSet<PathBuf> = current
        .iter()
        .filter_map(|path| {
            if path.is_file() {
                Some(normalize_resource_path(path.clone()))
            } else {
                None
            }
        })
        .collect();
    // Directory defaults need to expand to individual files before a delta
    // can remove one member. If no explicit files were present, start with
    // every discovered file, matching the default autoload state.
    if selected.is_empty() && current.iter().any(|path| path.is_dir()) {
        selected.extend(all.iter().cloned());
    }
    for pattern in patterns {
        let (mode, target) = pattern_mode(pattern);
        let exact = matches!(mode, PatternMode::ForceInclude | PatternMode::ForceExclude);
        for path in &all {
            if !matches_resource_pattern(path, &pattern_root, target, exact, kind) {
                continue;
            }
            match mode {
                PatternMode::Exclude | PatternMode::ForceExclude => {
                    selected.remove(path);
                }
                PatternMode::Include | PatternMode::ForceInclude => {
                    selected.insert(path.clone());
                }
            }
        }
    }
    sorted_paths(selected.into_iter().collect())
}

#[derive(Debug, Clone, Copy)]
enum PatternMode {
    Include,
    Exclude,
    ForceInclude,
    ForceExclude,
}

fn pattern_mode(pattern: &str) -> (PatternMode, &str) {
    if let Some(value) = pattern.strip_prefix('+') {
        (PatternMode::ForceInclude, value)
    } else if let Some(value) = pattern.strip_prefix('-') {
        (PatternMode::ForceExclude, value)
    } else if let Some(value) = pattern.strip_prefix('!') {
        (PatternMode::Exclude, value)
    } else {
        (PatternMode::Include, pattern)
    }
}

fn resource_inventory(root: &Path, defaults: &[PathBuf], kind: FilterResourceKind) -> Vec<PathBuf> {
    let Ok(boundary) = std::fs::canonicalize(root).map(normalize_resource_path) else {
        return Vec::new();
    };
    let mut out = HashSet::new();
    let mut visited = HashSet::new();
    for path in defaults {
        collect_resource_files(&boundary, path, kind, &mut out, &mut visited);
    }
    sorted_paths(out.into_iter().collect())
}

fn collect_resource_files(
    boundary: &Path,
    path: &Path,
    kind: FilterResourceKind,
    out: &mut HashSet<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) {
    let Some(canonical) = validated_resource_path(boundary, path) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.is_file() {
        if valid_resource_file(path, kind) {
            out.insert(canonical);
        }
        return;
    }
    if !metadata.is_dir() || !visited.insert(canonical) {
        return;
    }
    match kind {
        FilterResourceKind::Extensions => collect_extension_directory(boundary, path, out, visited),
        FilterResourceKind::Skills => collect_skill_directory(boundary, path, path, out, visited),
        FilterResourceKind::Prompts | FilterResourceKind::Themes => {
            collect_recursive_resource_directory(boundary, path, kind, out, visited)
        }
    }
}

fn valid_resource_file(path: &Path, kind: FilterResourceKind) -> bool {
    match kind {
        FilterResourceKind::Extensions => matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("js" | "ts")
        ),
        FilterResourceKind::Skills | FilterResourceKind::Prompts => {
            path.extension().and_then(|ext| ext.to_str()) == Some("md")
        }
        FilterResourceKind::Themes => path.extension().and_then(|ext| ext.to_str()) == Some("json"),
    }
}

fn collect_extension_directory(
    boundary: &Path,
    dir: &Path,
    out: &mut HashSet<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) {
    if let Some(entries) = extension_manifest_entries(dir) {
        let resolved = resolve_manifest_resources(
            boundary,
            dir,
            &entries,
            FilterResourceKind::Extensions,
            visited,
        );
        if resolved.had_source {
            out.extend(resolved.paths);
            return;
        }
    }

    for index in ["index.ts", "index.js"] {
        let path = dir.join(index);
        if let Some(canonical) = validated_resource_path(boundary, &path).filter(|_| path.is_file())
        {
            out.insert(canonical);
            return;
        }
    }

    for entry in visible_directory_entries(dir) {
        let path = entry.path();
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_file() {
            if valid_resource_file(&path, FilterResourceKind::Extensions) {
                if let Some(canonical) = validated_resource_path(boundary, &path) {
                    out.insert(canonical);
                }
            }
        } else if metadata.is_dir() {
            let Some(canonical) = validated_resource_path(boundary, &path) else {
                continue;
            };
            if !visited.insert(canonical) {
                continue;
            }
            collect_extension_entry_directory(boundary, &path, out, visited);
        }
    }
}

fn collect_extension_entry_directory(
    boundary: &Path,
    dir: &Path,
    out: &mut HashSet<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) {
    if let Some(entries) = extension_manifest_entries(dir) {
        let resolved = resolve_manifest_resources(
            boundary,
            dir,
            &entries,
            FilterResourceKind::Extensions,
            visited,
        );
        if resolved.had_source {
            out.extend(resolved.paths);
            return;
        }
    }
    for index in ["index.ts", "index.js"] {
        let path = dir.join(index);
        if let Some(canonical) = validated_resource_path(boundary, &path).filter(|_| path.is_file())
        {
            out.insert(canonical);
            return;
        }
    }
}

fn extension_manifest_entries(dir: &Path) -> Option<Vec<String>> {
    let manifest = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let manifest = parse_json_with_comments(&manifest).ok()?;
    let rpi = manifest.get("rpi").unwrap_or(&Value::Null);
    let pi = manifest.get("pi").unwrap_or(&Value::Null);
    let entries = rpi
        .get("extensions")
        .or_else(|| pi.get("extensions"))
        .or_else(|| manifest.get("extensions"))
        .map(string_values)?;
    (!entries.is_empty()).then_some(entries)
}

fn collect_skill_directory(
    boundary: &Path,
    dir: &Path,
    discovery_root: &Path,
    out: &mut HashSet<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) {
    let skill_file = dir.join("SKILL.md");
    if let Some(canonical) =
        validated_resource_path(boundary, &skill_file).filter(|_| skill_file.is_file())
    {
        out.insert(canonical);
        return;
    }

    for entry in visible_directory_entries(dir) {
        let path = entry.path();
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_file() {
            if dir == discovery_root && valid_resource_file(&path, FilterResourceKind::Skills) {
                if let Some(canonical) = validated_resource_path(boundary, &path) {
                    out.insert(canonical);
                }
            }
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        let Some(canonical) = validated_resource_path(boundary, &path) else {
            continue;
        };
        if visited.insert(canonical) {
            collect_skill_directory(boundary, &path, discovery_root, out, visited);
        }
    }
}

fn collect_recursive_resource_directory(
    boundary: &Path,
    dir: &Path,
    kind: FilterResourceKind,
    out: &mut HashSet<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) {
    for entry in visible_directory_entries(dir) {
        collect_resource_files(boundary, &entry.path(), kind, out, visited);
    }
}

fn visible_directory_entries(dir: &Path) -> Vec<std::fs::DirEntry> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| !name.starts_with('.') && name != "node_modules")
        })
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    entries
}

#[derive(Debug)]
struct ManifestResourceResolution {
    paths: Vec<PathBuf>,
    had_source: bool,
}

fn resolve_manifest_resources(
    boundary: &Path,
    base: &Path,
    entries: &[String],
    kind: FilterResourceKind,
    visited: &mut HashSet<PathBuf>,
) -> ManifestResourceResolution {
    let mut discovered = HashSet::new();
    let mut had_source = false;
    for entry in entries.iter().filter(|entry| !is_override_pattern(entry)) {
        let sources = if has_glob_pattern(entry) {
            expand_resource_glob(boundary, base, entry)
        } else {
            safe_resource_path_from(boundary, base, entry)
                .into_iter()
                .collect()
        };
        had_source |= !sources.is_empty();
        for source in sources {
            collect_resource_files(boundary, &source, kind, &mut discovered, visited);
        }
    }
    let all = sorted_paths(discovered.into_iter().collect());
    let patterns: Vec<String> = entries
        .iter()
        .filter(|entry| is_override_pattern(entry))
        .cloned()
        .collect();
    let base =
        normalize_resource_path(std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf()));
    let paths = apply_resource_patterns(&all, &patterns, &base, kind);
    ManifestResourceResolution { paths, had_source }
}

fn is_override_pattern(pattern: &str) -> bool {
    pattern.starts_with(['!', '+', '-'])
}

fn has_glob_pattern(pattern: &str) -> bool {
    pattern.contains(['*', '?'])
}

fn expand_resource_glob(boundary: &Path, base: &Path, pattern: &str) -> Vec<PathBuf> {
    let pattern = normalize_pattern(pattern);
    if pattern.is_empty()
        || Path::new(&pattern).is_absolute()
        || Path::new(&pattern)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Vec::new();
    }
    let Some(matcher) = compile_resource_glob(&pattern) else {
        return Vec::new();
    };
    let Some(canonical_base) = validated_resource_path(boundary, base) else {
        return Vec::new();
    };
    if !canonical_base.is_dir() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    let mut visited = HashSet::from([canonical_base]);
    walk_resource_glob(boundary, base, base, &matcher, &mut matches, &mut visited);
    sorted_paths(matches)
}

fn walk_resource_glob(
    boundary: &Path,
    base: &Path,
    dir: &Path,
    matcher: &globset::GlobMatcher,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) {
    for entry in visible_directory_entries_including_node_modules(dir) {
        let path = normalize_resource_path(entry.path());
        let Some(canonical) = validated_resource_path(boundary, &path) else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let Some(relative) = path.strip_prefix(base).ok().map(path_to_pattern) else {
            continue;
        };
        if matcher.is_match(&relative)
            || (metadata.is_dir() && matcher.is_match(format!("{relative}/")))
        {
            out.push(path.clone());
        }
        if metadata.is_dir() && visited.insert(canonical) {
            walk_resource_glob(boundary, base, &path, matcher, out, visited);
        }
    }
}

fn visible_directory_entries_including_node_modules(dir: &Path) -> Vec<std::fs::DirEntry> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| !name.starts_with('.'))
        })
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    entries
}

fn compile_resource_glob(pattern: &str) -> Option<globset::GlobMatcher> {
    let mut builder = globset::GlobBuilder::new(pattern);
    builder.literal_separator(true).backslash_escape(false);
    builder.build().ok().map(|glob| glob.compile_matcher())
}

fn apply_resource_patterns(
    all: &[PathBuf],
    patterns: &[String],
    base: &Path,
    kind: FilterResourceKind,
) -> Vec<PathBuf> {
    let includes: Vec<&str> = patterns
        .iter()
        .filter(|pattern| !is_override_pattern(pattern))
        .map(String::as_str)
        .collect();
    let excludes: Vec<&str> = patterns
        .iter()
        .filter_map(|pattern| pattern.strip_prefix('!'))
        .collect();
    let force_includes: Vec<&str> = patterns
        .iter()
        .filter_map(|pattern| pattern.strip_prefix('+'))
        .collect();
    let force_excludes: Vec<&str> = patterns
        .iter()
        .filter_map(|pattern| pattern.strip_prefix('-'))
        .collect();

    let mut selected: HashSet<PathBuf> = all
        .iter()
        .filter(|path| {
            includes.is_empty()
                || includes
                    .iter()
                    .any(|pattern| matches_resource_pattern(path, base, pattern, false, kind))
        })
        .cloned()
        .collect();
    if !excludes.is_empty() {
        selected.retain(|path| {
            !excludes
                .iter()
                .any(|pattern| matches_resource_pattern(path, base, pattern, false, kind))
        });
    }
    for path in all {
        if force_includes
            .iter()
            .any(|pattern| matches_resource_pattern(path, base, pattern, true, kind))
        {
            selected.insert(path.clone());
        }
    }
    if !force_excludes.is_empty() {
        selected.retain(|path| {
            !force_excludes
                .iter()
                .any(|pattern| matches_resource_pattern(path, base, pattern, true, kind))
        });
    }
    sorted_paths(selected.into_iter().collect())
}

fn sorted_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

fn matches_resource_pattern(
    path: &Path,
    root: &Path,
    pattern: &str,
    exact: bool,
    kind: FilterResourceKind,
) -> bool {
    let pattern = normalize_pattern(pattern);
    if pattern.is_empty() {
        return false;
    }
    let root =
        normalize_resource_path(std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()));
    let rel = path
        .strip_prefix(&root)
        .ok()
        .map(path_to_pattern)
        .unwrap_or_default();
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let absolute = path_to_pattern(path);
    let parent_rel = path
        .parent()
        .and_then(|parent| parent.strip_prefix(&root).ok())
        .map(path_to_pattern);
    let parent_absolute = path.parent().map(path_to_pattern);
    let exact_match =
        |candidate: &str| candidate == pattern || normalize_pattern(candidate) == pattern;
    if exact {
        return exact_match(&rel)
            || exact_match(&absolute)
            || (matches!(kind, FilterResourceKind::Skills)
                && (parent_rel.as_deref().is_some_and(exact_match)
                    || parent_absolute.as_deref().is_some_and(exact_match)));
    }
    let matcher = compile_resource_glob(&pattern);
    let matches = |candidate: &str| {
        matcher
            .as_ref()
            .is_some_and(|matcher| matcher.is_match(candidate))
            || exact_match(candidate)
    };
    matches(&rel)
        || matches(name)
        || matches(&absolute)
        || (matches!(kind, FilterResourceKind::Skills)
            && (parent_rel.as_deref().is_some_and(matches)
                || parent_absolute.as_deref().is_some_and(matches)))
}

fn normalize_pattern(pattern: &str) -> String {
    let normalized = pattern.trim().replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_string()
}

fn path_to_pattern(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn load_package(
    root: PathBuf,
    spec: &str,
    cwd: &Path,
    scope: ResolveScope,
    filter: Option<&crate::settings::PackageFilter>,
) -> Result<PackageRoot, String> {
    load_package_with_legacy_root(root, spec, cwd, scope, filter, None)
}

fn load_package_with_legacy_root(
    root: PathBuf,
    spec: &str,
    cwd: &Path,
    scope: ResolveScope,
    filter: Option<&crate::settings::PackageFilter>,
    legacy_npm_root: Option<PathBuf>,
) -> Result<PackageRoot, String> {
    let manifest_path = root.join("package.json");
    let raw =
        match std::fs::read_to_string(&manifest_path) {
            Ok(text) => Some(parse_json_with_comments(&text).map_err(|e| {
                format!("invalid package manifest {}: {e}", manifest_path.display())
            })?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("could not read {}: {e}", manifest_path.display())),
        };
    let manifest_name = raw
        .as_ref()
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str);
    let explicit_npm_source = if spec.trim_start().starts_with("npm:") {
        Some(
            npm_source_from_spec(spec)
                .ok_or_else(|| format!("invalid npm package source `{spec}`"))?,
        )
    } else {
        None
    };
    if legacy_npm_root.is_some() || explicit_npm_source.is_some() {
        let source = explicit_npm_source
            .as_ref()
            .ok_or_else(|| format!("legacy npm package has invalid source `{spec}`"))?;
        let expected = parse_npm_package_spec(spec)
            .map(|parsed| parsed.manifest_name)
            .ok_or_else(|| format!("invalid npm package source `{spec}`"))?;
        let actual = manifest_name.ok_or_else(|| {
            format!(
                "npm package manifest {} has no string package name; expected `{expected}`",
                manifest_path.display()
            )
        })?;
        if !npm_source_matches_manifest(source, actual) {
            return Err(format!(
                "npm package manifest name `{actual}` does not match configured package `{expected}`"
            ));
        }
    }
    let name = manifest_name
        .map(str::to_owned)
        .or_else(|| root.file_name().and_then(|s| s.to_str()).map(str::to_owned))
        .unwrap_or_else(|| spec.to_string());
    let version = raw
        .as_ref()
        .and_then(|v| v.get("version"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let manifest = raw.as_ref().map(|_| manifest_path);
    let source = if legacy_npm_root.is_some() {
        // A legacy lookup grants read-only provenance only after the manifest
        // identity check above. Updates still migrate into a managed store.
        explicit_npm_source
            .clone()
            .expect("legacy npm sources were validated above")
    } else {
        classify_package_source(&root, spec, &name, cwd, scope)
    };
    if explicit_npm_source.is_some()
        && is_managed_package_path(&root, cwd, scope)
        && source == PackageSource::Unknown
    {
        return Err(format!(
            "managed npm package provenance does not match configured source `{spec}`"
        ));
    }
    let npm_install_root = matches!(source, PackageSource::Npm { .. })
        .then(|| npm_install_root_for_path(&root, cwd, scope))
        .flatten();
    let git_store_root = matches!(source, PackageSource::Git)
        .then(|| native_git_store_root_for_path(&root, cwd, scope))
        .flatten();
    let git_revision = matches!(source, PackageSource::Git)
        .then(|| parse_git_source(spec).and_then(|git| git.revision))
        .flatten();
    // rpi-specific manifest settings win per resource key; a missing rpi key
    // falls back to the original Pi key so partial migrations stay compatible.
    let rpi = raw
        .as_ref()
        .and_then(|v| v.get("rpi"))
        .unwrap_or(&Value::Null);
    let pi = raw
        .as_ref()
        .and_then(|v| v.get("pi"))
        .unwrap_or(&Value::Null);

    let mut skills = resource_paths(
        &root,
        raw.as_ref(),
        rpi,
        pi,
        "skills",
        "skills",
        FilterResourceKind::Skills,
    );
    let mut prompts = resource_paths(
        &root,
        raw.as_ref(),
        rpi,
        pi,
        "prompts",
        "prompts",
        FilterResourceKind::Prompts,
    );
    let mut themes = resource_paths(
        &root,
        raw.as_ref(),
        rpi,
        pi,
        "themes",
        "themes",
        FilterResourceKind::Themes,
    );
    let mut extensions = resource_paths(
        &root,
        raw.as_ref(),
        rpi,
        pi,
        "extensions",
        "extensions",
        FilterResourceKind::Extensions,
    );
    let system_prompts = file_paths(
        &root,
        raw.as_ref(),
        rpi,
        pi,
        &["systemPrompt", "system_prompt", "system"],
        "SYSTEM.md",
    );
    let append_system_prompts = file_paths(
        &root,
        raw.as_ref(),
        rpi,
        pi,
        &["appendSystemPrompt", "append_system_prompt", "appendSystem"],
        "APPEND_SYSTEM.md",
    );
    let autoload_delta = filter.is_some_and(|filter| filter.autoload == Some(false));
    if let Some(filter) = filter {
        apply_package_filter(
            &root,
            &mut extensions,
            &mut skills,
            &mut prompts,
            &mut themes,
            filter,
        );
    }

    Ok(PackageRoot {
        skills,
        prompts,
        themes,
        system_prompts,
        append_system_prompts,
        extensions,
        root,
        name,
        version,
        manifest,
        spec: spec.to_string(),
        source,
        npm_install_root,
        legacy_npm_root,
        autoload_delta,
        scope,
        git_store_root,
        git_revision,
        missing_install: false,
        filter: filter.cloned(),
    })
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PackageSourceMarker {
    kind: String,
    spec: String,
}

pub(crate) fn write_npm_source_marker(root: &Path, spec: &str) -> Result<(), String> {
    let source = npm_source_from_spec(spec)
        .ok_or_else(|| format!("invalid npm package source marker spec `{spec}`"))?;
    let PackageSource::Npm { spec, .. } = source else {
        unreachable!();
    };
    let marker = PackageSourceMarker {
        kind: "npm".to_string(),
        spec,
    };
    let data = serde_json::to_vec_pretty(&marker).map_err(|error| error.to_string())?;
    std::fs::write(root.join(PACKAGE_SOURCE_MARKER), data)
        .map_err(|error| format!("could not write package source marker: {error}"))
}

pub(crate) fn remove_package_source_marker(root: &Path) -> Result<(), String> {
    match std::fs::remove_file(root.join(PACKAGE_SOURCE_MARKER)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove package source marker: {error}")),
    }
}

fn read_npm_source_marker(root: &Path, manifest_name: &str) -> Option<PackageSource> {
    let marker: PackageSourceMarker =
        serde_json::from_str(&std::fs::read_to_string(root.join(PACKAGE_SOURCE_MARKER)).ok()?)
            .ok()?;
    if marker.kind != "npm" {
        return None;
    }
    let source = npm_source_from_spec(&marker.spec)?;
    npm_source_matches_manifest(&source, manifest_name).then_some(source)
}

fn classify_package_source(
    root: &Path,
    spec: &str,
    manifest_name: &str,
    cwd: &Path,
    scope: ResolveScope,
) -> PackageSource {
    if let Some(raw) = spec.strip_prefix("npm:") {
        let Some(explicit_source) = npm_source_from_spec(&format!("npm:{raw}")) else {
            return PackageSource::Unknown;
        };
        if !npm_source_matches_manifest(&explicit_source, manifest_name) {
            return PackageSource::Unknown;
        }
        if is_npm_store_package_path(root, cwd, scope) {
            return explicit_source;
        }
        if is_managed_package_path(root, cwd, scope) {
            return read_npm_source_marker(root, manifest_name)
                .filter(|marker_source| marker_source == &explicit_source)
                .unwrap_or(PackageSource::Unknown);
        }
        return PackageSource::Unknown;
    }
    if let Some(git) = parse_git_source(spec) {
        return native_git_target_for_spec(cwd, scope, &git)
            .filter(|target| target == root)
            .map_or(PackageSource::Unknown, |_| PackageSource::Git);
    }
    if spec.starts_with("file:") || Path::new(spec).is_absolute() || spec.starts_with('.') {
        if is_native_git_package_path(root, cwd, scope) {
            return PackageSource::Git;
        }
        if is_direct_managed_package_root(root, cwd, scope).is_some() {
            return PackageSource::Git;
        }
        if is_managed_package_path(root, cwd, scope) || is_npm_store_package_path(root, cwd, scope)
        {
            if let Some(source) = read_npm_source_marker(root, manifest_name) {
                return source;
            }
        }
        if is_npm_store_package_path(root, cwd, scope) && valid_npm_name(manifest_name) {
            return PackageSource::Npm {
                name: manifest_name.to_string(),
                spec: format!("npm:{manifest_name}"),
                requested: None,
                pinned: false,
            };
        }
        return PackageSource::Local;
    }
    if is_npm_store_package_path(root, cwd, scope) && valid_npm_name(manifest_name) {
        return PackageSource::Npm {
            name: manifest_name.to_string(),
            spec: format!("npm:{manifest_name}"),
            requested: None,
            pinned: false,
        };
    }
    PackageSource::Unknown
}

fn npm_source_from_spec(spec: &str) -> Option<PackageSource> {
    let raw = spec.strip_prefix("npm:")?;
    let parsed = parse_npm_package_spec(raw)?;
    let pinned = parsed
        .target_selector
        .as_deref()
        .is_some_and(is_exact_npm_version);
    Some(PackageSource::Npm {
        name: parsed.install_name,
        spec: format!("npm:{}", raw.trim()),
        requested: parsed.requested,
        pinned,
    })
}

fn npm_source_matches_manifest(source: &PackageSource, manifest_name: &str) -> bool {
    let PackageSource::Npm { spec, .. } = source else {
        return false;
    };
    parse_npm_package_spec(spec).is_some_and(|parsed| parsed.manifest_name == manifest_name)
}

fn is_exact_npm_version(value: &str) -> bool {
    let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
    let mut build_parts = value.split('+');
    let core_and_pre = build_parts.next().unwrap_or_default();
    if build_parts
        .next()
        .is_some_and(|build| !valid_semver_identifiers(build, false))
        || build_parts.next().is_some()
    {
        return false;
    }
    let (core, prerelease) = core_and_pre
        .split_once('-')
        .map_or((core_and_pre, None), |(core, pre)| (core, Some(pre)));
    if prerelease.is_some_and(|pre| !valid_semver_identifiers(pre, true)) {
        return false;
    }
    let mut parts = core.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && [major, minor, patch].iter().all(|part| {
            !part.is_empty()
                && part.chars().all(|ch| ch.is_ascii_digit())
                && (*part == "0" || !part.starts_with('0'))
        })
}

fn valid_semver_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
                && (!reject_numeric_leading_zero
                    || !identifier.chars().all(|ch| ch.is_ascii_digit())
                    || identifier == "0"
                    || !identifier.starts_with('0'))
        })
}

pub(crate) fn parse_npm_package_spec(spec: &str) -> Option<ParsedNpmPackageSpec> {
    let raw = spec.strip_prefix("npm:").unwrap_or(spec).trim();
    let (install_name, requested) = split_npm_name_and_selector(raw)?;
    if !valid_npm_name(install_name) {
        return None;
    }

    let Some(requested) = requested else {
        return Some(ParsedNpmPackageSpec {
            install_name: install_name.to_string(),
            manifest_name: install_name.to_string(),
            requested: None,
            target_selector: None,
            is_alias: false,
        });
    };
    let requested = requested.trim();
    if !safe_npm_registry_selector(requested, true) {
        return None;
    }

    let Some(alias_target) = requested.strip_prefix("npm:") else {
        return Some(ParsedNpmPackageSpec {
            install_name: install_name.to_string(),
            manifest_name: install_name.to_string(),
            requested: Some(requested.to_string()),
            target_selector: Some(requested.to_string()),
            is_alias: false,
        });
    };
    let (manifest_name, target_selector) = split_npm_name_and_selector(alias_target)?;
    if !valid_npm_name(manifest_name)
        || target_selector.is_some_and(|selector| !safe_npm_registry_selector(selector, false))
    {
        return None;
    }
    Some(ParsedNpmPackageSpec {
        install_name: install_name.to_string(),
        manifest_name: manifest_name.to_string(),
        requested: Some(requested.to_string()),
        target_selector: target_selector.map(str::to_string),
        is_alias: true,
    })
}

fn split_npm_name_and_selector(spec: &str) -> Option<(&str, Option<&str>)> {
    let spec = spec.trim();
    if spec.is_empty() || spec.chars().any(char::is_control) {
        return None;
    }
    let separator = if spec.starts_with('@') {
        let slash = spec.find('/')?;
        spec[slash + 1..].find('@').map(|index| slash + 1 + index)
    } else {
        spec.find('@')
    };
    match separator {
        Some(index) => {
            let selector = &spec[index + 1..];
            (!selector.is_empty()).then_some((&spec[..index], Some(selector)))
        }
        None => Some((spec, None)),
    }
}

fn safe_npm_registry_selector(selector: &str, allow_alias: bool) -> bool {
    let selector = selector.trim();
    if selector.is_empty()
        || selector.starts_with('-')
        || selector.starts_with('.')
        || selector.starts_with('/')
        || selector.contains('\\')
        || selector.chars().any(char::is_control)
    {
        return false;
    }
    if let Some(target) = selector.strip_prefix("npm:") {
        return allow_alias && !target.is_empty();
    }
    let lower = selector.to_ascii_lowercase();
    ![
        "file:",
        "link:",
        "workspace:",
        "git:",
        "git+",
        "http:",
        "https:",
        "ssh:",
        "github:",
        "gitlab:",
        "bitbucket:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn valid_npm_name(name: &str) -> bool {
    if name.starts_with('-') {
        return false;
    }
    if let Some(scoped) = name.strip_prefix('@') {
        let mut parts = scoped.split('/');
        return parts.next().is_some_and(valid_npm_name_part)
            && parts.next().is_some_and(valid_npm_name_part)
            && parts.next().is_none();
    }
    valid_npm_name_part(name)
}

fn valid_npm_name_part(part: &str) -> bool {
    !matches!(part, "" | "." | "..")
        && !part.starts_with('.')
        && !part.starts_with('-')
        && part
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~'))
}

fn parse_json_with_comments(text: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str(text) {
        Ok(value) => Ok(value),
        Err(first) => serde_json::from_str(&config::strip_line_comments(text)).map_err(|_| first),
    }
}

fn resource_paths(
    root: &Path,
    top: Option<&Value>,
    rpi: &Value,
    pi: &Value,
    key: &str,
    default_dir: &str,
    kind: FilterResourceKind,
) -> Vec<PathBuf> {
    let values = rpi
        .get(key)
        .or_else(|| pi.get(key))
        .or_else(|| top.and_then(|v| v.get(key)));
    if let Some(values) = values {
        return resolve_manifest_resources(
            root,
            root,
            &string_values(values),
            kind,
            &mut HashSet::new(),
        )
        .paths;
    }
    resource_inventory(root, &[root.join(default_dir)], kind)
}

fn file_paths(
    root: &Path,
    top: Option<&Value>,
    rpi: &Value,
    pi: &Value,
    keys: &[&str],
    default_file: &str,
) -> Vec<PathBuf> {
    let value = keys.iter().find_map(|key| {
        rpi.get(*key)
            .or_else(|| pi.get(*key))
            .or_else(|| top.and_then(|v| v.get(*key)))
    });
    let mut paths = value
        .map(|v| {
            string_values(v)
                .into_iter()
                .filter_map(|path| safe_resource_path(root, &path))
                .collect()
        })
        .unwrap_or_else(|| vec![root.join(default_file)]);
    paths.retain(|p: &PathBuf| p.is_file());
    paths
}

fn string_values(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn normalize_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RestoreEnv {
        name: &'static str,
        value: Option<std::ffi::OsString>,
    }

    impl RestoreEnv {
        fn capture(name: &'static str) -> Self {
            Self {
                name,
                value: std::env::var_os(name),
            }
        }
    }

    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match self.value.take() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn package_command_trust_overrides_are_explicit_and_conflict_safe() {
        let cwd = Path::new(".");
        assert!(package_command_project_trusted(cwd, &["--approve".into()]).unwrap());
        assert!(!package_command_project_trusted(cwd, &["--no-approve".into()]).unwrap());
        assert!(
            package_command_project_trusted(cwd, &["--approve".into(), "--no-approve".into()])
                .is_err()
        );
        assert!(package_command_project_trusted(cwd, &["--unexpected".into()]).is_err());
    }

    #[test]
    fn package_update_accepts_offline_flag_and_skips_all_preflight() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let _restore_config = RestoreEnv::capture(config::CONFIG_DIR_ENV);
        let _restore_offline = RestoreEnv::capture(crate::args::PI_OFFLINE_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("native-packages.json"), "{ malformed").unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);
        std::env::remove_var(crate::args::PI_OFFLINE_ENV);

        assert_eq!(run_cli(&["update".into(), "--offline".into()]), 0);
        assert_eq!(
            std::env::var(crate::args::PI_OFFLINE_ENV).as_deref(),
            Ok("1")
        );

        // A non-truthy value must not silently suppress the same invalid
        // registry preflight.
        std::env::set_var(crate::args::PI_OFFLINE_ENV, "0");
        assert_eq!(
            update_packages_with_scope(tmp.path(), false, UpdateScope::Native),
            1
        );
    }

    #[test]
    fn top_level_update_help_is_handled_by_package_updater() {
        assert_eq!(run_cli(&["update".into(), "--help".into()]), 0);
    }

    #[test]
    fn native_and_pi_update_scopes_validate_only_their_own_metadata() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("native-packages.json"), "[{broken").unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        assert_eq!(
            update_packages_with_scope(tmp.path(), false, UpdateScope::Native),
            1
        );
        assert_eq!(
            update_packages_with_scope(tmp.path(), false, UpdateScope::Pi),
            0
        );

        std::fs::remove_file(agent.join("native-packages.json")).unwrap();
        std::fs::write(agent.join("settings.json"), "{ malformed").unwrap();
        assert_eq!(
            update_packages_with_scope(tmp.path(), false, UpdateScope::Native),
            0
        );
        assert_eq!(
            update_packages_with_scope(tmp.path(), false, UpdateScope::Pi),
            1
        );

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn discovers_conventional_and_manifest_resources() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        std::fs::create_dir_all(root.join("custom-skills")).unwrap();
        std::fs::create_dir_all(root.join("rpi-skills")).unwrap();
        std::fs::create_dir_all(root.join("prompts")).unwrap();
        std::fs::create_dir_all(root.join("legacy-prompts")).unwrap();
        std::fs::create_dir_all(root.join("themes")).unwrap();
        std::fs::write(root.join("custom-skills/a.md"), "---\nname: a\n---\nbody").unwrap();
        std::fs::write(root.join("rpi-skills/rpi.md"), "---\nname: rpi\n---\nbody").unwrap();
        std::fs::write(root.join("prompts/explain.md"), "explain").unwrap();
        std::fs::write(root.join("legacy-prompts/legacy.md"), "legacy").unwrap();
        std::fs::write(root.join("themes/ocean.json"), "{}").unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"demo","version":"1.0.0","pi":{"skills":["custom-skills"],"prompts":["legacy-prompts"]},"rpi":{"skills":["rpi-skills"]}}"#,
        )
        .unwrap();

        let resources = discover(tmp.path(), &[root.to_string_lossy().into_owned()]);
        assert_eq!(resources.packages.len(), 1);
        assert_eq!(resources.packages[0].name, "demo");
        assert_eq!(resources.skill_dirs(), vec![root.join("rpi-skills/rpi.md")]);
        assert_eq!(
            resources.prompt_dirs(),
            vec![root.join("legacy-prompts/legacy.md")]
        );
        assert_eq!(
            resources.theme_files(),
            vec![root.join("themes/ocean.json")]
        );
    }

    #[test]
    fn manifest_globs_and_overrides_follow_native_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        for dir in [
            root.join("extensions"),
            root.join("plugins/one/skills/alpha"),
            root.join("plugins/two/skills/beta"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for path in ["extensions/a.ts", "extensions/z.ts"] {
            std::fs::write(root.join(path), "export default () => {};").unwrap();
        }
        for path in [
            "plugins/one/skills/alpha/SKILL.md",
            "plugins/two/skills/beta/SKILL.md",
        ] {
            std::fs::write(root.join(path), "---\nname: demo\n---\n").unwrap();
        }
        std::fs::write(
            root.join("package.json"),
            r#"{
                "name":"glob-demo",
                "pi":{
                    "extensions":[
                        "extensions/*.ts",
                        "!**/*.ts",
                        "+extensions/a.ts",
                        "-extensions/z.ts",
                        "+extensions/z.ts"
                    ],
                    "skills":["plugins/*/skills"]
                }
            }"#,
        )
        .unwrap();

        let resources = discover(tmp.path(), &[root.to_string_lossy().into_owned()]);
        assert_eq!(
            resources.extension_paths(),
            vec![root.join("extensions/a.ts")]
        );
        assert_eq!(
            resources.skill_dirs(),
            vec![
                root.join("plugins/one/skills/alpha/SKILL.md"),
                root.join("plugins/two/skills/beta/SKILL.md"),
            ]
        );
    }

    #[test]
    fn extension_directories_use_smart_entry_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        for dir in [
            root.join("extensions/group"),
            root.join("extensions/custom"),
            root.join("extensions/broken"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for (path, body) in [
            ("extensions/standalone.ts", "export default () => {};"),
            ("extensions/group/index.ts", "export default () => {};"),
            ("extensions/group/helper.ts", "export const helper = 1;"),
            ("extensions/custom/main.js", "export default () => {};"),
            ("extensions/custom/utils.js", "export const util = 1;"),
            ("extensions/broken/helper.ts", "export const helper = 1;"),
        ] {
            std::fs::write(root.join(path), body).unwrap();
        }
        std::fs::write(
            root.join("extensions/custom/package.json"),
            r#"{"pi":{"extensions":["main.js"]}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"smart-demo","pi":{"extensions":["extensions"]}}"#,
        )
        .unwrap();

        let package = load_package(
            root.clone(),
            &root.to_string_lossy(),
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap();
        assert_eq!(
            package.extensions,
            vec![
                root.join("extensions/custom/main.js"),
                root.join("extensions/group/index.ts"),
                root.join("extensions/standalone.ts"),
            ]
        );

        std::fs::write(root.join("extensions/index.js"), "export default () => {};").unwrap();
        let package = load_package(
            root.clone(),
            &root.to_string_lossy(),
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap();
        assert_eq!(package.extensions, vec![root.join("extensions/index.js")]);
    }

    #[test]
    fn skill_directory_discovery_ignores_nested_markdown_helpers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        for dir in [
            root.join("skills/group/nested"),
            root.join("skills/docs"),
            root.join("skills/deep/alpha"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for path in [
            "skills/root.md",
            "skills/group/SKILL.md",
            "skills/group/README.md",
            "skills/group/nested/SKILL.md",
            "skills/docs/README.md",
            "skills/deep/alpha/SKILL.md",
        ] {
            std::fs::write(root.join(path), "---\nname: demo\n---\n").unwrap();
        }
        std::fs::write(root.join("package.json"), r#"{"name":"skill-demo"}"#).unwrap();

        let package = load_package(
            root.clone(),
            &root.to_string_lossy(),
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap();
        assert_eq!(
            package.skills,
            vec![
                root.join("skills/deep/alpha/SKILL.md"),
                root.join("skills/group/SKILL.md"),
                root.join("skills/root.md"),
            ]
        );
    }

    #[test]
    fn manifest_prompt_and_theme_directories_are_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        std::fs::create_dir_all(root.join("prompt-pack/nested")).unwrap();
        std::fs::create_dir_all(root.join("theme-pack/nested")).unwrap();
        std::fs::write(root.join("prompt-pack/root.md"), "root").unwrap();
        std::fs::write(root.join("prompt-pack/nested/deep.md"), "deep").unwrap();
        std::fs::write(root.join("theme-pack/root.json"), "{}").unwrap();
        std::fs::write(root.join("theme-pack/nested/deep.json"), "{}").unwrap();
        std::fs::write(root.join("theme-pack/nested/not-theme.md"), "ignored").unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{
                "name":"recursive-demo",
                "pi":{"prompts":["prompt-pack"],"themes":["theme-pack"]}
            }"#,
        )
        .unwrap();

        let resources = discover(tmp.path(), &[root.to_string_lossy().into_owned()]);
        assert_eq!(
            resources.prompt_dirs(),
            vec![
                root.join("prompt-pack/nested/deep.md"),
                root.join("prompt-pack/root.md"),
            ]
        );
        assert_eq!(
            resources.theme_files(),
            vec![
                root.join("theme-pack/nested/deep.json"),
                root.join("theme-pack/root.json"),
            ]
        );
    }

    #[test]
    fn manifest_resource_paths_cannot_escape_the_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("outside.ts"), "export default () => {};").unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"escape-demo","pi":{"extensions":["../outside/outside.ts","../outside/*.ts"]}}"#,
        )
        .unwrap();

        let package = load_package(
            root.clone(),
            &root.to_string_lossy(),
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap();
        assert!(package.extensions.is_empty());
    }

    #[test]
    fn manifest_resource_symlink_escape_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        let outside = tmp.path().join("outside.ts");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, "export default () => {};").unwrap();
        let link = root.join("linked.ts");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&outside, &link).is_err() {
            return;
        }
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"symlink-demo","pi":{"extensions":["linked.ts"]}}"#,
        )
        .unwrap();

        let package = load_package(
            root.clone(),
            &root.to_string_lossy(),
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap();
        assert!(package.extensions.is_empty());
    }

    #[test]
    fn resolves_package_json_spec_and_deduplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        let manifest = root.join("package.json").to_string_lossy().into_owned();
        let resources = discover(
            tmp.path(),
            &[manifest.clone(), root.to_string_lossy().into_owned()],
        );
        assert_eq!(resources.packages.len(), 1);
        assert!(resources.diagnostics.is_empty());
    }

    #[test]
    fn bare_package_name_prefers_project_rpi_store_over_legacy_pi_store() {
        let tmp = tempfile::tempdir().unwrap();
        let rpi_root = tmp.path().join(".rpi/packages/demo");
        let pi_root = tmp.path().join(".pi/packages/demo");
        std::fs::create_dir_all(rpi_root.join("skills")).unwrap();
        std::fs::create_dir_all(pi_root.join("skills")).unwrap();
        std::fs::write(
            rpi_root.join("package.json"),
            r#"{"name":"rpi-demo","version":"rpi"}"#,
        )
        .unwrap();
        std::fs::write(
            pi_root.join("package.json"),
            r#"{"name":"pi-demo","version":"pi"}"#,
        )
        .unwrap();

        let resources = discover(tmp.path(), &["demo".to_string()]);
        assert_eq!(resources.packages.len(), 1);
        assert_eq!(resources.packages[0].root, rpi_root);
        assert_eq!(resources.packages[0].version.as_deref(), Some("rpi"));
    }

    #[test]
    fn npm_scoped_spec_resolves_installed_safe_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".rpi/packages/narumitw__pi-btw");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"@narumitw/pi-btw","version":"0.58.1"}"#,
        )
        .unwrap();
        write_npm_source_marker(&root, "npm:@narumitw/pi-btw").unwrap();

        let resources = discover(tmp.path(), &["npm:@narumitw/pi-btw".to_string()]);
        assert_eq!(resources.packages.len(), 1);
        assert!(resources.diagnostics.is_empty());
        assert_eq!(resources.packages[0].name, "@narumitw/pi-btw");
    }

    #[test]
    fn npm_scoped_spec_resolves_project_store_and_versioned_spec() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".pi/npm/node_modules/@scope/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"@scope/demo","version":"1.2.3"}"#,
        )
        .unwrap();

        for spec in ["npm:@scope/demo", "npm:@scope/demo@1.2.3"] {
            let resources = discover(tmp.path(), &[spec.to_string()]);
            assert!(resources.diagnostics.is_empty(), "spec={spec}");
            assert_eq!(resources.packages[0].root, root, "spec={spec}");
        }
    }

    #[test]
    fn npm_store_detection_is_bounded_to_the_configured_agent_root() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("real-agent");
        let package = agent.join("npm/node_modules/@scope/demo");
        let impostor = tmp
            .path()
            .join("workspace/agent/npm/node_modules/@scope/demo");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::create_dir_all(&impostor).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        assert!(is_npm_store_package_path(
            &package,
            tmp.path(),
            ResolveScope::Any
        ));
        assert_eq!(
            npm_install_root_for_path(&package, tmp.path(), ResolveScope::Any),
            std::fs::canonicalize(agent.join("npm")).ok()
        );
        assert!(!is_npm_store_package_path(
            &impostor,
            tmp.path(),
            ResolveScope::Any
        ));
        assert!(!is_npm_store_package_path(
            &agent.join("npm/node_modules"),
            tmp.path(),
            ResolveScope::Any
        ));
        let nested = package.join("node_modules/dependency");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(!is_npm_store_package_path(
            &nested,
            tmp.path(),
            ResolveScope::Any
        ));

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn legacy_global_npm_is_discovered_but_updates_only_in_managed_store() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let global_root = tmp.path().join("legacy-global/node_modules");
        let global_package = global_root.join("demo");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(global_package.join("extensions")).unwrap();
        std::fs::write(
            global_package.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            global_package.join("extensions/index.js"),
            "export default () => {};",
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resolved =
            resolve_spec_with_legacy_lookup(&cwd, "npm:demo", ResolveScope::User, |name| {
                assert_eq!(name, "demo");
                std::fs::canonicalize(&global_package).ok()
            })
            .unwrap();
        let canonical_global_root = std::fs::canonicalize(&global_root).unwrap();
        assert_eq!(
            resolved.root,
            std::fs::canonicalize(&global_package).unwrap()
        );
        assert_eq!(
            resolved.legacy_npm_root.as_deref(),
            Some(canonical_global_root.as_path())
        );

        let package = load_package_with_legacy_root(
            resolved.root,
            "npm:demo",
            &cwd,
            ResolveScope::User,
            None,
            resolved.legacy_npm_root,
        )
        .unwrap();
        assert_eq!(package.updateable_npm_name(), Some("demo"));
        assert!(package.npm_install_root.is_none());
        let update_root = package
            .npm_store_root_for_update(&cwd, false)
            .unwrap()
            .unwrap();
        assert_eq!(update_root, agent.join("npm"));
        assert_ne!(update_root, global_root);
        assert!(!is_managed_package_path(
            &global_package,
            &cwd,
            ResolveScope::User
        ));

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn static_managed_npm_precedes_legacy_lookup() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let package = agent.join("npm/node_modules/demo");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resolved =
            resolve_spec_with_legacy_lookup(tmp.path(), "npm:demo", ResolveScope::User, |_| {
                panic!("legacy global lookup must not run for a managed package")
            })
            .unwrap();
        assert_eq!(resolved.root, package);
        assert!(resolved.legacy_npm_root.is_none());

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn legacy_global_lookup_is_never_used_for_project_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let global_root = tmp.path().join("legacy/node_modules/demo");
        std::fs::create_dir_all(&global_root).unwrap();
        std::fs::write(global_root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        let resolved = resolve_spec_with_legacy_lookup(
            &tmp.path().join("project"),
            "npm:demo",
            ResolveScope::Project,
            |_| panic!("project scope must not consult a global package manager"),
        );
        assert!(resolved.is_none());
    }

    #[test]
    fn legacy_manifest_name_mismatch_blocks_package_loading() {
        let tmp = tempfile::tempdir().unwrap();
        let global_root = tmp.path().join("legacy/node_modules");
        let package_root = global_root.join("demo");
        std::fs::create_dir_all(&package_root).unwrap();
        std::fs::write(
            package_root.join("package.json"),
            r#"{"name":"other","version":"1.0.0"}"#,
        )
        .unwrap();
        let error = load_package_with_legacy_root(
            std::fs::canonicalize(&package_root).unwrap(),
            "npm:demo",
            tmp.path(),
            ResolveScope::User,
            None,
            std::fs::canonicalize(&global_root).ok(),
        )
        .unwrap_err();
        assert!(error.contains("manifest name `other`"), "{error}");
        assert!(error.contains("configured package `demo`"), "{error}");
    }

    #[test]
    fn legacy_npm_without_manifest_identity_blocks_package_loading() {
        let tmp = tempfile::tempdir().unwrap();
        let global_root = tmp.path().join("legacy/node_modules");
        let package_root = global_root.join("demo");
        std::fs::create_dir_all(&package_root).unwrap();
        std::fs::write(package_root.join("package.json"), r#"{"version":"1.0.0"}"#).unwrap();

        let error = load_package_with_legacy_root(
            std::fs::canonicalize(&package_root).unwrap(),
            "npm:demo",
            tmp.path(),
            ResolveScope::User,
            None,
            std::fs::canonicalize(&global_root).ok(),
        )
        .unwrap_err();

        assert!(error.contains("has no string package name"), "{error}");
        assert!(error.contains("expected `demo`"), "{error}");
    }

    #[test]
    fn filtered_package_entries_apply_only_the_requested_resources() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let root = agent.join("packages/demo");
        std::fs::create_dir_all(root.join("extensions")).unwrap();
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::fs::write(root.join("extensions/index.js"), "export default () => {};").unwrap();
        std::fs::write(root.join("skills/review.md"), "review").unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        write_npm_source_marker(&root, "npm:demo@beta").unwrap();
        std::fs::write(
            agent.join("settings.json"),
            r#"{"packages":[{"source":"npm:demo@beta","autoload":false,"extensions":["+extensions/index.js"]}]}"#,
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resources = discover_from_global_settings(tmp.path());

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(resources.packages.len(), 1);
        assert_eq!(
            resources.packages[0].updateable_npm_source(),
            Some(("demo", "npm:demo@beta"))
        );
        assert_eq!(
            resources.extension_paths(),
            vec![root.join("extensions/index.js")]
        );
        assert!(resources.skill_dirs().is_empty());
        assert!(resources.diagnostics.is_empty());
    }

    #[test]
    fn filtered_package_entries_do_not_disable_other_resource_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("package");
        std::fs::create_dir_all(root.join("extensions")).unwrap();
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::fs::create_dir_all(root.join("prompts")).unwrap();
        std::fs::create_dir_all(root.join("themes")).unwrap();
        for (path, body) in [
            ("extensions/a.js", "export default () => {};"),
            ("skills/keep.md", "keep"),
            ("skills/drop.md", "drop"),
            ("prompts/one.md", "one"),
            ("themes/one.json", "{}"),
        ] {
            std::fs::write(root.join(path), body).unwrap();
        }
        std::fs::write(root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        let filter = crate::settings::PackageFilter {
            source: root.to_string_lossy().into_owned(),
            autoload: None,
            extensions: Some(Vec::new()),
            skills: Some(vec!["skills/keep.md".to_string()]),
            prompts: None,
            themes: None,
            unknown: serde_json::Map::new(),
        };
        let package = load_package(
            root.clone(),
            &filter.source,
            tmp.path(),
            ResolveScope::Any,
            Some(&filter),
        )
        .unwrap();
        assert!(package.extensions.is_empty());
        assert_eq!(package.skills, vec![root.join("skills/keep.md")]);
        assert_eq!(package.prompts, vec![root.join("prompts/one.md")]);
        assert_eq!(package.themes, vec![root.join("themes/one.json")]);
    }

    #[test]
    fn project_autoload_delta_keeps_matching_global_package_resources() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let root = tmp.path().join("shared-package");
        std::fs::create_dir_all(root.join("extensions")).unwrap();
        std::fs::write(root.join("extensions/a.js"), "export default () => {}; ").unwrap();
        std::fs::write(root.join("extensions/b.js"), "export default () => {}; ").unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"shared"}"#).unwrap();
        let spec = format!("file:{}", root.display());
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({"packages":[spec]})).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(cwd.join(".rpi")).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "packages":[{"source":spec,"autoload":false,"extensions":["+extensions/a.js"]}]
            }))
            .unwrap(),
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);
        let resources = discover_from_settings(&cwd);
        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(resources.packages.len(), 1);
        assert!(!resources.packages[0].autoload_delta);
        assert_eq!(resources.extension_paths().len(), 2);
    }

    #[test]
    fn configured_packages_with_same_manifest_name_keep_distinct_local_roots() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        for root in [&first, &second] {
            std::fs::create_dir_all(root.join("skills")).unwrap();
            std::fs::write(root.join("skills/item.md"), "item").unwrap();
            std::fs::write(root.join("package.json"), r#"{"name":"same"}"#).unwrap();
        }
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            serde_json::to_vec(
                &serde_json::json!({"packages":[format!("file:{}", second.display())]}),
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(cwd.join(".rpi")).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            serde_json::to_vec(
                &serde_json::json!({"packages":[format!("file:{}", first.display())]}),
            )
            .unwrap(),
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);
        let resources = discover_from_settings(&cwd);
        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(resources.packages.len(), 2);
        assert_eq!(resources.packages[0].root, first);
        assert_eq!(resources.packages[1].root, second);
    }

    #[test]
    fn project_relative_package_paths_resolve_from_pi_config_directory() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let previous_config = std::env::var_os(config::CONFIG_DIR_ENV);
        let isolated_agent = tmp.path().join("agent");
        std::fs::create_dir_all(&isolated_agent).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &isolated_agent);
        let cwd = tmp.path().join("project");
        let root = cwd.join(".pi/packages/demo");
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::fs::write(root.join("skills/item.md"), "item").unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        std::fs::write(
            cwd.join(".pi/settings.json"),
            r#"{"packages":["./packages/demo"]}"#,
        )
        .unwrap();
        let resources = discover_from_settings(&cwd);
        match previous_config {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(resources.packages.len(), 1);
        assert_eq!(resources.packages[0].root, root);
    }

    #[test]
    fn native_git_sources_resolve_only_inside_pi_git_store() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);
        let cwd = tmp.path().join("project");
        let root = cwd.join(".pi/git/github.com/example/repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::fs::write(root.join("skills/item.md"), "item").unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"repo"}"#).unwrap();
        std::fs::create_dir_all(cwd.join(".pi")).unwrap();
        std::fs::write(
            cwd.join(".pi/settings.json"),
            r#"{"packages":["git:https://github.com/example/repo.git@main"]}"#,
        )
        .unwrap();
        let resources = discover_from_settings(&cwd);
        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(resources.packages.len(), 1);
        assert_eq!(resources.packages[0].source, PackageSource::Git);
        assert_eq!(resources.packages[0].git_revision.as_deref(), Some("main"));
    }

    #[test]
    fn git_sources_preserve_slash_refs_across_supported_transports() {
        let cases = [
            (
                "git:github.com/example/repo@feature/branch",
                "github.com",
                "example/repo",
                Some("feature/branch"),
            ),
            (
                "https://github.com/example/repo.git@feature/branch",
                "github.com",
                "example/repo",
                Some("feature/branch"),
            ),
            (
                "ssh://git@github.com/example/repo@release/v2",
                "github.com",
                "example/repo",
                Some("release/v2"),
            ),
            (
                "git://github.com/example/repo.git@refs/heads/main",
                "github.com",
                "example/repo",
                Some("refs/heads/main"),
            ),
            (
                "git:git@github.com:example/repo@hotfix/security",
                "github.com",
                "example/repo",
                Some("hotfix/security"),
            ),
        ];
        for (spec, host, path, revision) in cases {
            let parsed = parse_git_source(spec).unwrap_or_else(|| panic!("spec={spec}"));
            assert_eq!(parsed.host, host, "spec={spec}");
            assert_eq!(parsed.path, path, "spec={spec}");
            assert_eq!(parsed.revision.as_deref(), revision, "spec={spec}");
        }
    }

    #[test]
    fn git_source_parser_rejects_encoded_traversal_and_unsafe_refs() {
        for spec in [
            "git:git@evil.example:../../victim/repo",
            "https://evil.example/..%2F..%2Fvictim/repo",
            "git:github.com/example/repo@../escape",
            "git:github.com/example/repo@-upload-pack=evil",
            "git:github.com/example/repo@feature\\branch",
            "git:github.com/example/repo@feature%2F..%2Fescape",
        ] {
            assert!(parse_git_source(spec).is_none(), "spec={spec}");
        }
    }

    #[test]
    fn git_source_parser_preserves_remote_transport_authority() {
        let shorthand = parse_git_source("git:github.com/example/repo").unwrap();
        assert_eq!(shorthand.transport, GitTransport::Https);
        assert_eq!(shorthand.port, None);
        assert_eq!(shorthand.user_info, None);

        let https = parse_git_source("https://token@github.com:8443/example/repo.git").unwrap();
        assert_eq!(https.transport, GitTransport::Https);
        assert_eq!(https.port, Some(8443));
        assert_eq!(https.user_info.as_deref(), Some("token"));

        let scp = parse_git_source("git:git@github.com:example/repo").unwrap();
        assert_eq!(scp.transport, GitTransport::Ssh);
        assert_eq!(scp.port, None);
        assert_eq!(scp.user_info.as_deref(), Some("git"));

        for invalid in [
            "https://github.com:70000/example/repo",
            "https://user @github.com/example/repo",
        ] {
            assert!(parse_git_source(invalid).is_none(), "spec={invalid}");
        }
    }

    #[test]
    fn pinned_git_packages_are_selected_for_manual_updates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".pi/git/github.com/example/repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"repo"}"#).unwrap();
        let package = load_package(
            root,
            "git:github.com/example/repo@feature/branch",
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap();
        assert_eq!(package.source, PackageSource::Git);
        assert_eq!(package.git_revision.as_deref(), Some("feature/branch"));
        assert!(package.updateable_git_source());
    }

    #[test]
    fn missing_npm_update_targets_use_native_managed_roots_and_keep_pins() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let project =
            missing_package_for_update(&cwd, "npm:@scope/demo@beta", ResolveScope::Project, None)
                .unwrap()
                .unwrap();
        assert!(project.missing_install);
        assert_eq!(project.name, "@scope/demo");
        assert_eq!(project.root, cwd.join(".pi/npm/node_modules/@scope/demo"));
        assert_eq!(project.npm_install_root, Some(cwd.join(".pi/npm")));
        assert_eq!(
            project.updateable_npm_source(),
            Some(("@scope/demo", "npm:@scope/demo@beta"))
        );

        let user = missing_package_for_update(&cwd, "npm:demo@1.2.3", ResolveScope::User, None)
            .unwrap()
            .unwrap();
        assert_eq!(user.root, agent.join("npm/node_modules/demo"));
        assert_eq!(user.npm_install_root, Some(agent.join("npm")));
        assert_eq!(
            user.updateable_npm_source(),
            Some(("demo", "npm:demo@1.2.3"))
        );

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn missing_git_update_target_preserves_ref_and_scope() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let project = missing_package_for_update(
            &cwd,
            "git:github.com/example/repo@feature/branch",
            ResolveScope::Project,
            None,
        )
        .unwrap()
        .unwrap();
        assert!(project.missing_install);
        assert!(project.updateable_git_source());
        assert_eq!(project.name, "repo");
        assert_eq!(project.root, cwd.join(".pi/git/github.com/example/repo"));
        assert_eq!(project.git_store_root, Some(cwd.join(".pi/git")));
        assert_eq!(project.git_revision.as_deref(), Some("feature/branch"));
        assert_eq!(
            update_recovery_targets(
                &cwd,
                "git:github.com/example/repo@feature/branch",
                ResolveScope::Project,
            ),
            vec![
                cwd.join(".rpi/git/github.com/example/repo"),
                cwd.join(".pi/git/github.com/example/repo"),
            ]
        );

        let user = missing_package_for_update(
            &cwd,
            "https://github.com/example/other.git@release/v2",
            ResolveScope::User,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(user.root, agent.join("git/github.com/example/other"));
        assert_eq!(user.git_store_root, Some(agent.join("git")));
        assert_eq!(user.git_revision.as_deref(), Some("release/v2"));
        let mut recovery_targets = vec![agent.join("git/github.com/example/other")];
        if let Some(home) = dirs::home_dir() {
            recovery_targets.push(home.join(".pi/agent/git/github.com/example/other"));
        }
        assert_eq!(
            update_recovery_targets(
                &cwd,
                "https://github.com/example/other.git@release/v2",
                ResolveScope::User,
            ),
            recovery_targets
        );

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn update_discovery_represents_missing_registry_and_git_sources_only() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let entries = [
            "npm:demo@latest",
            "npm:fixed@1.2.3",
            "git:github.com/example/repo@main",
            "file:missing-local",
        ]
        .into_iter()
        .map(|source| crate::settings::PackageSetting::from(source.to_string()))
        .collect::<Vec<_>>();

        let resources =
            discover_with_scope_and_command(&cwd, &entries, ResolveScope::Project, true, None);
        assert_eq!(resources.packages.len(), 3);
        assert!(resources
            .packages
            .iter()
            .all(|package| package.missing_install));
        assert!(resources.diagnostics.is_empty());

        let ordinary =
            discover_with_scope_and_command(&cwd, &entries, ResolveScope::Project, false, None);
        assert!(ordinary.packages.is_empty());
        assert_eq!(ordinary.diagnostics.len(), entries.len());
    }

    #[test]
    fn git_metadata_requires_a_real_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let git_file = tmp.path().join(".git");
        std::fs::write(&git_file, "gitdir: ../outside/.git\n").unwrap();
        assert!(!is_real_git_metadata(&git_file));
        std::fs::remove_file(&git_file).unwrap();
        std::fs::create_dir(&git_file).unwrap();
        assert!(is_real_git_metadata(&git_file));
    }

    #[test]
    fn marker_source_updates_ranges_and_tags_but_skips_exact_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".rpi/packages/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();

        let file_spec = format!("file:{}", root.display());
        for (source_spec, updateable) in [
            ("npm:demo@1.0.0", false),
            ("npm:demo@1.0.0-beta.1", false),
            ("npm:demo@^1", true),
            ("npm:demo@latest", true),
            ("npm:demo@beta", true),
            ("npm:demo", true),
        ] {
            write_npm_source_marker(&root, source_spec).unwrap();
            let package = load_package(
                root.clone(),
                &file_spec,
                tmp.path(),
                ResolveScope::Any,
                None,
            )
            .unwrap();
            assert_eq!(
                package.updateable_npm_name().is_some(),
                updateable,
                "source_spec={source_spec}"
            );
        }
    }

    #[test]
    fn explicit_npm_in_ordinary_node_modules_is_never_updateable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("node_modules/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        write_npm_source_marker(&root, "npm:demo").unwrap();
        let package = load_package(root, "npm:demo", tmp.path(), ResolveScope::Any, None).unwrap();
        assert_eq!(package.source, PackageSource::Unknown);
        assert_eq!(package.updateable_npm_name(), None);
    }

    #[test]
    fn managed_npm_marker_must_match_manifest_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".rpi/packages/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        write_npm_source_marker(&root, "npm:other").unwrap();
        let spec = format!("file:{}", root.display());
        let package = load_package(root, &spec, tmp.path(), ResolveScope::Any, None).unwrap();
        assert_eq!(package.source, PackageSource::Local);
    }

    #[test]
    fn explicit_npm_source_must_match_marker_and_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".rpi/packages/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        write_npm_source_marker(&root, "npm:demo@beta").unwrap();

        let matching = load_package(
            root.clone(),
            "npm:demo@beta",
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap();
        assert_eq!(
            matching.updateable_npm_source(),
            Some(("demo", "npm:demo@beta"))
        );

        let wrong_name = load_package(
            root.clone(),
            "npm:other@beta",
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap_err();
        assert!(wrong_name.contains("manifest name `demo`"), "{wrong_name}");

        let wrong_selector =
            load_package(root, "npm:demo@^1", tmp.path(), ResolveScope::Any, None).unwrap_err();
        assert!(wrong_selector.contains("provenance"), "{wrong_selector}");
    }

    #[test]
    fn managed_npm_alias_marker_target_mismatch_blocks_loading() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".rpi/packages/alias");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"real","version":"1.2.3"}"#,
        )
        .unwrap();
        write_npm_source_marker(&root, "npm:alias@npm:other@^1").unwrap();

        let error = load_package(
            root,
            "npm:alias@npm:real@^1",
            tmp.path(),
            ResolveScope::Any,
            None,
        )
        .unwrap_err();

        assert!(error.contains("provenance"), "{error}");
    }

    #[test]
    fn explicit_native_npm_without_manifest_identity_is_not_loadable() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let root = cwd.join(".pi/npm/node_modules/demo");
        std::fs::create_dir_all(root.join("extensions")).unwrap();
        std::fs::write(root.join("extensions/index.js"), "export default () => {};").unwrap();
        let entries = [crate::settings::PackageSetting::from(
            "npm:demo".to_string(),
        )];

        for manifest in [None, Some(r#"{"version":"1.0.0"}"#)] {
            if let Some(manifest) = manifest {
                std::fs::write(root.join("package.json"), manifest).unwrap();
            }
            let resources =
                discover_with_scope_and_command(&cwd, &entries, ResolveScope::Project, false, None);
            assert!(resources.packages.is_empty(), "manifest={manifest:?}");
            assert!(
                resources.extension_paths().is_empty(),
                "manifest={manifest:?}"
            );
            assert_eq!(resources.diagnostics.len(), 1, "manifest={manifest:?}");
            assert!(
                resources.diagnostics[0]
                    .message
                    .contains("has no string package name"),
                "{}",
                resources.diagnostics[0].message
            );
        }
    }

    #[test]
    fn managed_file_entry_remains_updateable_after_changing_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project-a/.rpi/packages/demo");
        let other_cwd = tmp.path().join("project-b");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other_cwd).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        write_npm_source_marker(&root, "npm:demo@beta").unwrap();
        let spec = format!("file:{}", root.display());

        let package = load_package(root, &spec, &other_cwd, ResolveScope::User, None).unwrap();
        assert_eq!(
            package.updateable_npm_source(),
            Some(("demo", "npm:demo@beta"))
        );
    }

    #[test]
    fn legacy_file_entry_for_managed_git_clone_keeps_git_provenance() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".rpi/packages/demo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        let spec = format!("file:{}", root.display());
        let package = load_package(root, &spec, tmp.path(), ResolveScope::Any, None).unwrap();
        assert_eq!(package.source, PackageSource::Git);
    }

    #[test]
    fn native_scoped_npm_root_has_registry_provenance() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join(".pi/agent");
        let root = agent.join("npm/node_modules/@scope/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"@scope/demo","version":"1.0.0"}"#,
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);
        let package =
            load_package(root, "@scope/demo", tmp.path(), ResolveScope::Any, None).unwrap();
        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(package.updateable_npm_name(), Some("@scope/demo"));
    }

    #[test]
    fn exact_semver_and_npm_name_validation_are_conservative() {
        for version in ["1.2.3", "v1.2.3", "1.2.3-beta.1", "1.2.3+build"] {
            assert!(is_exact_npm_version(version), "version={version}");
        }
        for version in ["1", "1.2", "^1.2.3", "latest", "1.2.3-", "1.02.3"] {
            assert!(!is_exact_npm_version(version), "version={version}");
        }
        for name in [
            "-rf",
            "--workspace",
            "@scope/..",
            "@scope/a\\b",
            "@scope/a?b",
            "a b",
            "a#b",
        ] {
            assert!(!valid_npm_name(name), "name={name}");
        }
    }

    #[test]
    fn npm_alias_parser_separates_install_slot_from_manifest_name() {
        for (spec, install_name, manifest_name, requested, target_selector) in [
            ("alias@npm:real", "alias", "real", "npm:real", None),
            (
                "npm:@scope/alias@npm:real@^1",
                "@scope/alias",
                "real",
                "npm:real@^1",
                Some("^1"),
            ),
            (
                "alias@npm:@target/real@beta",
                "alias",
                "@target/real",
                "npm:@target/real@beta",
                Some("beta"),
            ),
            (
                "@scope/alias@npm:@target/real@1.2.3",
                "@scope/alias",
                "@target/real",
                "npm:@target/real@1.2.3",
                Some("1.2.3"),
            ),
        ] {
            let parsed = parse_npm_package_spec(spec).unwrap_or_else(|| panic!("spec={spec}"));
            assert_eq!(parsed.install_name, install_name, "spec={spec}");
            assert_eq!(parsed.manifest_name, manifest_name, "spec={spec}");
            assert_eq!(parsed.requested.as_deref(), Some(requested), "spec={spec}");
            assert_eq!(
                parsed.target_selector.as_deref(),
                target_selector,
                "spec={spec}"
            );
            assert!(parsed.is_alias, "spec={spec}");
        }

        for spec in [
            "alias@npm:real@npm:other",
            "alias@file:../real",
            "alias@npm:real@file:../other",
            "alias@git:https://example.com/repo.git",
            "alias@npm:",
            "@scope/alias@npm:@target/real@npm:other",
            "alias@npm:real\nlatest",
        ] {
            assert!(parse_npm_package_spec(spec).is_none(), "spec={spec}");
        }
    }

    #[test]
    fn managed_npm_alias_keeps_provenance_and_rejects_manifest_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".pi/npm/node_modules/@scope/alias");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"@target/real","version":"1.2.3"}"#,
        )
        .unwrap();
        let spec = "npm:@scope/alias@npm:@target/real@^1";

        let package =
            load_package(root.clone(), spec, tmp.path(), ResolveScope::Project, None).unwrap();
        assert_eq!(package.name, "@target/real");
        assert_eq!(package_identity(&package), "npm:@scope/alias");
        assert_eq!(
            package.updateable_npm_source(),
            Some(("@scope/alias", spec))
        );

        std::fs::write(
            root.join("package.json"),
            r#"{"name":"@target/wrong","version":"1.2.3"}"#,
        )
        .unwrap();
        let mismatched =
            load_package(root, spec, tmp.path(), ResolveScope::Project, None).unwrap_err();
        assert!(
            mismatched.contains("manifest name `@target/wrong`"),
            "{mismatched}"
        );
    }

    #[test]
    fn runtime_npm_alias_matches_target_selector_and_pinning() {
        let tmp = tempfile::tempdir().unwrap();
        for (slot, target, version, selector, needs_install, pinned) in [
            (
                "exact-match",
                "real-exact-match",
                "1.2.3",
                "1.2.3",
                false,
                true,
            ),
            (
                "exact-stale",
                "real-exact-stale",
                "1.2.4",
                "1.2.3",
                true,
                true,
            ),
            (
                "range-match",
                "real-range-match",
                "1.9.0",
                "^1.2.3",
                false,
                false,
            ),
            (
                "range-stale",
                "real-range-stale",
                "2.0.0",
                "^1.2.3",
                true,
                false,
            ),
            ("tag", "real-tag", "1.0.0", "beta", false, false),
        ] {
            let root = tmp.path().join(".pi/npm/node_modules").join(slot);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("package.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name": target,
                    "version": version
                }))
                .unwrap(),
            )
            .unwrap();
            let spec = format!("npm:{slot}@npm:{target}@{selector}");
            let package =
                load_package(root, &spec, tmp.path(), ResolveScope::Project, None).unwrap();

            assert_eq!(
                runtime_npm_needs_install(&package),
                needs_install,
                "spec={spec}"
            );
            assert_eq!(
                matches!(package.source, PackageSource::Npm { pinned: true, .. }),
                pinned,
                "spec={spec}"
            );
            assert_eq!(
                package.updateable_npm_source().is_some(),
                !pinned,
                "spec={spec}"
            );
        }
    }

    #[test]
    fn runtime_npm_version_matching_covers_native_common_ranges() {
        for (installed, requested, expected) in [
            (Some("1.2.3"), "1.2.3", Some(true)),
            (Some("1.2.4"), "1.2.3", Some(false)),
            (Some("1.9.0"), "^1.2.3", Some(true)),
            (Some("2.0.0"), "^1.2.3", Some(false)),
            (Some("1.2.9"), "~1.2.3", Some(true)),
            (Some("1.3.0"), "~1.2.3", Some(false)),
            (Some("1.2.9"), "1.2", Some(true)),
            (Some("1.3.0"), "1.2", Some(false)),
            (Some("1.5.0"), ">=1.2.0 <2.0.0", Some(true)),
            (Some("1.9.9"), ">= 2.0.0", Some(false)),
            (Some("2.0.0"), ">= 2.0.0", Some(true)),
            (Some("2.5.0"), ">= 2.0.0 < 3.0.0", Some(true)),
            (Some("3.0.0"), ">= 2.0.0 < 3.0.0", Some(false)),
            (Some("2.1.0"), "^1 || ^2", Some(true)),
            (Some("3.0.0"), "^1 || ^2", Some(false)),
            (Some("1.3.9"), "1.2 - 1.3", Some(true)),
            (Some("1.4.0"), "1.2 - 1.3", Some(false)),
            (Some("2.9.0"), "1 - 2", Some(true)),
            (Some("3.0.0"), "1 - 2", Some(false)),
            (Some("1.0.0"), "latest", None),
            (None, "1.2.3", Some(false)),
        ] {
            assert_eq!(
                npm_version_matches_requirement(installed, requested),
                expected,
                "installed={installed:?}, requested={requested}"
            );
        }
    }

    #[test]
    fn runtime_missing_exact_npm_fails_closed_for_invalid_command() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            r#"{"packages":["npm:demo@1.2.3"],"npmCommand":[""]}"#,
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resources = resolve_from_global_settings(&cwd);

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert!(resources.packages.is_empty());
        assert_eq!(resources.diagnostics.len(), 1);
        assert!(resources.diagnostics[0]
            .message
            .contains("invalid npmCommand"));
        assert!(!agent.join("npm/node_modules/demo").exists());
    }

    #[test]
    fn offline_runtime_quarantines_mismatched_npm_but_keeps_matching_range() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        for (name, version) in [("stale", "1.0.0"), ("matching", "1.5.0")] {
            let root = agent.join("npm/node_modules").join(name);
            std::fs::create_dir_all(root.join("extensions")).unwrap();
            std::fs::write(
                root.join("package.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name": name,
                    "version": version
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::write(root.join("extensions/index.js"), "export default () => {};").unwrap();
        }
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            r#"{
                "npmCommand":[""],
                "packages":["npm:stale@2.0.0","npm:matching@^1.0.0"]
            }"#,
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resources = resolve_offline_from_global_settings(&cwd);

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(resources.packages.len(), 1);
        assert_eq!(resources.packages[0].name, "matching");
        assert_eq!(resources.diagnostics.len(), 1);
        assert_eq!(resources.diagnostics[0].spec, "npm:stale@2.0.0");
        assert!(resources.diagnostics[0].message.contains("offline"));
    }

    #[test]
    fn offline_runtime_never_invokes_configured_npm_for_legacy_lookup() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let _restore_config = RestoreEnv::capture(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let marker = tmp.path().join("npm-command-ran");
        let script = tmp
            .path()
            .join(if cfg!(windows) { "npm.ps1" } else { "npm.sh" });
        let script_body = if cfg!(windows) {
            format!(
                "Set-Content -LiteralPath '{}' -Value invoked\nexit 0\n",
                marker.to_string_lossy().replace('\'', "''")
            )
        } else {
            format!(
                "printf invoked > '{}'\nexit 0\n",
                marker.to_string_lossy().replace('\'', "'\\''")
            )
        };
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(&script, script_body).unwrap();
        let npm_command = if cfg!(windows) {
            vec![
                "powershell.exe".to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-File".to_string(),
                script.to_string_lossy().into_owned(),
            ]
        } else {
            vec!["sh".to_string(), script.to_string_lossy().into_owned()]
        };
        std::fs::write(
            agent.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "npmCommand": npm_command,
                "packages": ["npm:missing-legacy-package"]
            }))
            .unwrap(),
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resources = resolve_offline_from_global_settings(&cwd);

        assert!(resources.packages.is_empty());
        assert_eq!(resources.diagnostics.len(), 1);
        assert!(
            !marker.exists(),
            "offline package discovery unexpectedly launched npmCommand"
        );
    }

    #[test]
    fn runtime_rechecks_version_after_a_noop_package_manager_success() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let root = agent.join("npm/node_modules/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        let noop_script = tmp
            .path()
            .join(if cfg!(windows) { "noop.ps1" } else { "noop.sh" });
        std::fs::write(&noop_script, "exit 0\n").unwrap();
        let command = if cfg!(windows) {
            vec![
                "powershell.exe",
                "-NoProfile",
                "-NonInteractive",
                "-File",
                noop_script.to_str().unwrap(),
            ]
        } else {
            vec!["sh", noop_script.to_str().unwrap()]
        };
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "npmCommand": command,
                "packages": ["npm:demo@2.0.0"]
            }))
            .unwrap(),
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resources = resolve_from_global_settings(&cwd);

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert!(resources.packages.is_empty());
        assert_eq!(resources.diagnostics.len(), 1);
        assert!(resources.diagnostics[0]
            .message
            .contains("still does not satisfy"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(root.join("package.json")).unwrap()
            )
            .unwrap()["version"],
            "1.0.0"
        );
    }

    #[test]
    fn global_npm_spec_cannot_be_shadowed_by_project_store_or_node_modules() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("user-agent");
        let cwd = tmp.path().join("workspace/project");
        let user = agent.join("npm/node_modules/demo");
        for root in [&user, &cwd.join(".rpi/packages/demo")] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        }
        let workspace_package = tmp.path().join("workspace/node_modules/demo");
        std::fs::create_dir_all(&workspace_package).unwrap();
        std::fs::write(workspace_package.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("settings.json"), r#"{"packages":["npm:demo"]}"#).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resources = discover_from_global_settings(&cwd);
        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(resources.packages.len(), 1);
        assert_eq!(resources.packages[0].root, user);
    }

    #[test]
    fn configured_project_and_user_packages_resolve_in_separate_scopes() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("user-agent");
        let cwd = tmp.path().join("project");
        let project_root = cwd.join(".rpi/packages/demo");
        let user_root = agent.join("packages/demo");
        for root in [&project_root, &user_root] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(
                root.join("package.json"),
                r#"{"name":"demo","version":"1.0.0"}"#,
            )
            .unwrap();
            write_npm_source_marker(root, "npm:demo").unwrap();
        }
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("settings.json"), r#"{"packages":["npm:demo"]}"#).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let user_only = discover_from_settings(&cwd);
        assert_eq!(user_only.packages.len(), 1);
        assert_eq!(user_only.packages[0].root, user_root);

        std::fs::create_dir_all(cwd.join(".rpi")).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            r#"{"packages":["npm:demo"]}"#,
        )
        .unwrap();
        let combined = discover_from_settings(&cwd);
        assert_eq!(combined.packages.len(), 1);
        assert_eq!(combined.packages[0].root, project_root);

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn update_discovery_keeps_same_identity_in_both_scopes() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let project_root = cwd.join(".pi/npm/node_modules/demo");
        let user_root = agent.join("npm/node_modules/demo");
        for root in [&project_root, &user_root] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(
                root.join("package.json"),
                r#"{"name":"demo","version":"1.0.0"}"#,
            )
            .unwrap();
        }
        std::fs::create_dir_all(cwd.join(".rpi")).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            r#"{"packages":["npm:demo@latest"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            r#"{"packages":["npm:demo@latest"]}"#,
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let resources = discover_from_settings_for_update(&cwd, true).unwrap().0;

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(resources.packages.len(), 2);
        assert!(resources
            .packages
            .iter()
            .any(|package| package.root == project_root));
        assert!(resources
            .packages
            .iter()
            .any(|package| package.root == user_root));
    }

    #[test]
    fn update_discovery_ignores_untrusted_project_settings_but_keeps_user_packages() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let user_root = agent.join("packages/user-demo");
        let project_root = cwd.join(".rpi/packages/project-demo");
        for (root, name) in [(&user_root, "user-demo"), (&project_root, "project-demo")] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(
                root.join("package.json"),
                serde_json::to_vec(&serde_json::json!({"name": name, "version": "1.0.0"})).unwrap(),
            )
            .unwrap();
            write_npm_source_marker(root, &format!("npm:{name}")).unwrap();
        }
        std::fs::write(
            agent.join("settings.json"),
            r#"{"packages":["npm:user-demo"]}"#,
        )
        .unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            r#"{"packages":["npm:project-demo"]}"#,
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let untrusted = discover_from_settings_for_update(&cwd, false).unwrap().0;
        let trusted = discover_from_settings_for_update(&cwd, true).unwrap().0;

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(untrusted.packages.len(), 1);
        assert_eq!(untrusted.packages[0].name, "user-demo");
        assert_eq!(trusted.packages.len(), 2);
        assert_eq!(trusted.packages[0].name, "project-demo");
        assert_eq!(trusted.packages[1].name, "user-demo");
    }

    #[test]
    fn update_discovery_recovers_missing_configured_target_and_cleans_stale_backup() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("user-agent");
        let cwd = tmp.path().join("project");
        let target = cwd.join(".rpi/packages/demo");
        let backup = cwd.join(".rpi/packages/.demo.rpi-backup-00000000000000000000000000000001");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(
            backup.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        write_npm_source_marker(&backup, "npm:demo@beta").unwrap();
        let spec = format!("file:{}", target.display());
        std::fs::create_dir_all(cwd.join(".rpi")).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            serde_json::to_vec(&serde_json::json!({ "packages": [spec] })).unwrap(),
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let ordinary = discover_from_settings(&cwd);
        assert!(ordinary.packages.is_empty());
        assert!(backup.is_dir());
        assert!(!target.exists());

        let recovered = discover_from_settings_for_update(&cwd, true).unwrap().0;
        assert_eq!(recovered.packages.len(), 1);
        assert_eq!(recovered.packages[0].root, target);
        assert!(!backup.exists());

        let stale = cwd.join(".rpi/packages/.demo.rpi-backup-00000000000000000000000000000002");
        std::fs::create_dir_all(&stale).unwrap();
        let visible = discover_from_settings_for_update(&cwd, true).unwrap().0;
        assert_eq!(visible.packages.len(), 1);
        assert!(!stale.exists());

        let next_backup =
            cwd.join(".rpi/packages/.demo.rpi-backup-00000000000000000000000000000003");
        std::fs::rename(&target, &next_backup).unwrap();
        let recovered_again = discover_from_settings_for_update(&cwd, true).unwrap().0;
        assert_eq!(recovered_again.packages.len(), 1);
        assert!(target.is_dir());
        assert!(!next_backup.exists());

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn update_discovery_recovers_native_project_and_user_npm_targets() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("user-agent");
        let cwd = tmp.path().join("project");
        let project_target = cwd.join(".pi/npm/node_modules/demo");
        let project_backup =
            cwd.join(".pi/npm/node_modules/.demo.rpi-backup-00000000000000000000000000000011");
        let user_target = agent.join("npm/node_modules/@scope/demo");
        let user_backup =
            agent.join("npm/node_modules/@scope/.demo.rpi-backup-00000000000000000000000000000012");
        for (backup, name) in [(&project_backup, "demo"), (&user_backup, "@scope/demo")] {
            std::fs::create_dir_all(backup).unwrap();
            std::fs::write(
                backup.join("package.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name": name,
                    "version": "1.0.0"
                }))
                .unwrap(),
            )
            .unwrap();
        }
        std::fs::create_dir_all(cwd.join(".rpi")).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            r#"{"packages":["npm:demo"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            r#"{"packages":["npm:@scope/demo"]}"#,
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        let ordinary = discover_from_settings(&cwd);
        assert!(ordinary.packages.is_empty());
        assert!(project_backup.is_dir());
        assert!(user_backup.is_dir());

        let recovered = discover_from_settings_for_update(&cwd, true).unwrap().0;
        assert_eq!(recovered.packages.len(), 2);
        assert!(recovered
            .packages
            .iter()
            .any(|package| package.root == project_target));
        assert!(recovered
            .packages
            .iter()
            .any(|package| package.root == user_target));
        assert!(!project_backup.exists());
        assert!(!user_backup.exists());

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn update_returns_failure_when_recovery_is_ambiguous() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("user-agent");
        let cwd = tmp.path().join("project");
        let target = cwd.join(".rpi/packages/demo");
        for suffix in [1_u8, 2] {
            let backup = cwd.join(format!(".rpi/packages/.demo.rpi-backup-{suffix:032x}"));
            std::fs::create_dir_all(&backup).unwrap();
            std::fs::write(backup.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        }
        let spec = format!("file:{}", target.display());
        std::fs::create_dir_all(cwd.join(".rpi")).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            serde_json::to_vec(&serde_json::json!({ "packages": [spec] })).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        assert_eq!(update_packages_with_scope(&cwd, true, UpdateScope::Pi), 1);
        assert!(!target.exists());

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
    }

    #[test]
    fn update_with_malformed_project_settings_performs_no_recovery() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let target = cwd.join(".rpi/packages/demo");
        let backup = cwd.join(".rpi/packages/.demo.rpi-backup-00000000000000000000000000000031");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        std::fs::write(cwd.join(".rpi/settings.json"), "{ malformed").unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        assert_eq!(update_packages_with_scope(&cwd, true, UpdateScope::Pi), 1);

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert!(backup.is_dir());
        assert!(!target.exists());
    }

    #[test]
    fn update_with_corrupt_native_registry_performs_no_ts_recovery() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let target = cwd.join(".rpi/packages/demo");
        let backup = cwd.join(".rpi/packages/.demo.rpi-backup-00000000000000000000000000000034");
        let metadata = agent.join("native-packages.json");
        let original = b"[{broken native metadata";
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(&metadata, original).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        let spec = format!("file:{}", target.display());
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            serde_json::to_vec(&serde_json::json!({ "packages": [spec] })).unwrap(),
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        assert_eq!(
            update_packages_with_scope(&cwd, true, UpdateScope::Native),
            1
        );

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert_eq!(std::fs::read(metadata).unwrap(), original);
        assert!(backup.is_dir());
        assert!(!target.exists());
    }

    #[test]
    fn update_with_malformed_global_settings_performs_no_project_recovery() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let target = cwd.join(".rpi/packages/demo");
        let backup = cwd.join(".rpi/packages/.demo.rpi-backup-00000000000000000000000000000032");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("settings.json"), "{ malformed").unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "packages": [format!("file:{}", target.display())]
            }))
            .unwrap(),
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        assert_eq!(update_packages_with_scope(&cwd, true, UpdateScope::Pi), 1);

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert!(backup.is_dir());
        assert!(!target.exists());
    }

    #[test]
    fn update_with_invalid_npm_command_performs_no_recovery() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let previous = std::env::var_os(config::CONFIG_DIR_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("agent");
        let cwd = tmp.path().join("project");
        let target = cwd.join(".rpi/packages/demo");
        let backup = cwd.join(".rpi/packages/.demo.rpi-backup-00000000000000000000000000000033");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("package.json"), r#"{"name":"demo"}"#).unwrap();
        std::fs::write(
            cwd.join(".rpi/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "npmCommand": [""],
                "packages": [format!("file:{}", target.display())]
            }))
            .unwrap(),
        )
        .unwrap();
        std::env::set_var(config::CONFIG_DIR_ENV, &agent);

        assert_eq!(update_packages_with_scope(&cwd, true, UpdateScope::Pi), 1);

        match previous {
            Some(value) => std::env::set_var(config::CONFIG_DIR_ENV, value),
            None => std::env::remove_var(config::CONFIG_DIR_ENV),
        }
        assert!(backup.is_dir());
        assert!(!target.exists());
    }
}
