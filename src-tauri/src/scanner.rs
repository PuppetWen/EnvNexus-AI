use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDriveStringsW};

const DISCOVERY_INDEX_SCHEMA: u32 = 1;
const PROBE_CACHE_SCHEMA: u32 = 1;
const DRIVE_FIXED: u32 = 3;

use crate::{
    environment::{
        EnvironmentMap, collect_candidate_roots, diagnose_environment, get_case_insensitive,
        read_environment, split_path,
    },
    error::AppResult,
    installer::{InstallManifest, managed_manifests, manifest_executables},
    model::{
        DiagnosticIssue, EnvironmentScan, EnvironmentScope, HealthLevel, InstalledVersion,
        IssueLevel, ToolCapabilities, ToolInventory, VersionManagerInventory,
    },
    plugins::{PluginRegistry, ToolDescriptor, ToolPlugin},
    process::{output_text, run_capture},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiskDiscoveryIndex {
    schema_version: u32,
    scanned_at: chrono::DateTime<Utc>,
    executables: HashMap<String, Vec<PathBuf>>,
    manifests: Vec<InstallManifest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolProbeCache {
    schema_version: u32,
    entries: HashMap<String, ToolProbeCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolProbeCacheEntry {
    tool_id: String,
    executable: PathBuf,
    fingerprint: ToolProbeFingerprint,
    version: String,
    installation_root: PathBuf,
    health: HealthLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolProbeFingerprint {
    executable_size: u64,
    executable_modified_millis: u64,
    companion_size: Option<u64>,
    companion_modified_millis: Option<u64>,
}

struct ToolProbeContext {
    cache: ToolProbeCache,
    touched_keys: HashSet<String>,
    reuse_cached: bool,
}

struct ToolScanSources<'a> {
    manifests: &'a [InstallManifest],
    configured_root: Option<&'a Path>,
    disk_executables: &'a [PathBuf],
    configured_executables: &'a [PathBuf],
}

pub fn scan(registry: &PluginRegistry, data_root: &Path) -> AppResult<EnvironmentScan> {
    scan_internal(registry, data_root, true)
}

pub fn refresh(registry: &PluginRegistry, data_root: &Path) -> AppResult<EnvironmentScan> {
    scan_internal(registry, data_root, false)
}

fn scan_internal(
    registry: &PluginRegistry,
    data_root: &Path,
    force_disk_discovery: bool,
) -> AppResult<EnvironmentScan> {
    let started = Utc::now();
    let user = read_environment(EnvironmentScope::User)?;
    let system = read_environment(EnvironmentScope::System)?;
    let mut global_issues = diagnose_environment(&user, &system);
    let mut tools = Vec::with_capacity(registry.all().len());
    let version_managers = detect_version_managers(&user, &system);
    let mut preferences = crate::read_tool_root_preferences(data_root)?;
    let discovery = disk_discovery(registry, data_root, force_disk_discovery);
    let configured_discovery =
        configured_root_discovery(registry, &preferences, force_disk_discovery);
    let mut probes = ToolProbeContext {
        cache: if force_disk_discovery {
            ToolProbeCache {
                schema_version: PROBE_CACHE_SCHEMA,
                entries: HashMap::new(),
            }
        } else {
            read_tool_probe_cache(registry, data_root)
        },
        touched_keys: HashSet::new(),
        reuse_cached: !force_disk_discovery,
    };
    let mut manifests = managed_manifests(data_root);
    merge_discovered_manifests(&mut manifests, discovery.manifests);
    merge_discovered_manifests(&mut manifests, configured_discovery.manifests);
    if recover_tool_roots(registry, &mut preferences, &manifests) {
        crate::write_tool_root_preferences(data_root, &preferences)?;
    }

    for plugin in registry.all() {
        let configured_root = preferences
            .roots
            .get(plugin.descriptor().id)
            .map(PathBuf::as_path);
        tools.push(scan_tool(
            plugin.as_ref(),
            &user,
            &system,
            ToolScanSources {
                manifests: &manifests,
                configured_root,
                disk_executables: discovery
                    .executables
                    .get(plugin.descriptor().id)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                configured_executables: configured_discovery
                    .executables
                    .get(plugin.descriptor().id)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            },
            &mut probes,
        ));
    }
    probes
        .cache
        .entries
        .retain(|key, entry| probes.touched_keys.contains(key) && entry.executable.is_file());
    write_tool_probe_cache(data_root, &probes.cache);
    add_version_manager_diagnostics(&tools, &version_managers, &mut global_issues);
    add_cross_tool_diagnostics(&tools, &user, &system, &mut global_issues);

    let user_path_entries = split_path(get_case_insensitive(&user, "PATH")).len();
    Ok(EnvironmentScan {
        tools,
        issues: global_issues,
        version_managers,
        user_path_entries,
        scan_started_at: started,
        scan_finished_at: Utc::now(),
    })
}

fn configured_root_discovery(
    registry: &PluginRegistry,
    preferences: &crate::model::ToolRootPreferences,
    full_disk_scan: bool,
) -> DiskDiscoveryIndex {
    let mut roots = preferences.roots.values().cloned().collect::<Vec<_>>();
    deduplicate_roots(&mut roots);
    if full_disk_scan {
        let fixed_roots = fixed_drive_roots();
        roots.retain(|root| {
            !fixed_roots
                .iter()
                .any(|fixed_root| root.starts_with(fixed_root))
        });
    }
    discover_disk_index_in_roots(registry, &roots)
}

fn tool_probe_cache_path(data_root: &Path) -> PathBuf {
    data_root.join("cache").join("tool-version-probes.json")
}

fn read_tool_probe_cache(registry: &PluginRegistry, data_root: &Path) -> ToolProbeCache {
    let Some(mut cache) = fs::read(tool_probe_cache_path(data_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ToolProbeCache>(&bytes).ok())
        .filter(|cache| cache.schema_version == PROBE_CACHE_SCHEMA)
    else {
        return ToolProbeCache {
            schema_version: PROBE_CACHE_SCHEMA,
            entries: HashMap::new(),
        };
    };
    cache.entries.retain(|_, entry| {
        if registry.get(&entry.tool_id).is_err() {
            return false;
        }
        entry.executable = crate::paths::simplify(std::mem::take(&mut entry.executable));
        entry.installation_root =
            crate::paths::simplify(std::mem::take(&mut entry.installation_root));
        entry.executable.is_file()
    });
    cache
}

fn write_tool_probe_cache(data_root: &Path, cache: &ToolProbeCache) {
    if let Ok(bytes) = serde_json::to_vec(cache) {
        let _ = crate::write_bytes_atomic(&tool_probe_cache_path(data_root), &bytes);
    }
}

fn disk_discovery(registry: &PluginRegistry, data_root: &Path, force: bool) -> DiskDiscoveryIndex {
    if !force && let Some(index) = read_disk_discovery_index(registry, data_root) {
        return index;
    }
    let index = discover_disk_index_in_roots(registry, &fixed_drive_roots());
    if let Ok(bytes) = serde_json::to_vec_pretty(&index) {
        let _ = crate::write_bytes_atomic(&disk_discovery_index_path(data_root), &bytes);
    }
    index
}

fn disk_discovery_index_path(data_root: &Path) -> PathBuf {
    data_root
        .join("cache")
        .join("tool-executable-discovery.json")
}

fn read_disk_discovery_index(
    registry: &PluginRegistry,
    data_root: &Path,
) -> Option<DiskDiscoveryIndex> {
    let bytes = fs::read(disk_discovery_index_path(data_root)).ok()?;
    let mut index = serde_json::from_slice::<DiskDiscoveryIndex>(&bytes).ok()?;
    if index.schema_version != DISCOVERY_INDEX_SCHEMA {
        return None;
    }
    index.executables.retain(|tool_id, paths| {
        if registry.get(tool_id).is_err() {
            return false;
        }
        for path in paths.iter_mut() {
            *path = crate::paths::simplify(std::mem::take(path));
        }
        paths.retain(|path| path.is_file());
        let mut seen = HashSet::new();
        paths.retain(|path| seen.insert(normalized_path_key(path)));
        !paths.is_empty()
    });
    index.manifests.retain(|manifest| {
        valid_discovered_manifest(
            &manifest.installation_path.join(".envpilot-install.json"),
            manifest,
        )
    });
    Some(index)
}

fn discover_disk_index_in_roots(
    registry: &PluginRegistry,
    roots: &[PathBuf],
) -> DiskDiscoveryIndex {
    let executable_names = registry
        .all()
        .iter()
        .map(|plugin| {
            (
                plugin.descriptor().executable.to_ascii_lowercase(),
                plugin.descriptor().id.to_string(),
            )
        })
        .collect::<HashMap<_, _>>();
    let descriptors = registry
        .all()
        .iter()
        .map(|plugin| (plugin.descriptor().id, plugin.descriptor()))
        .collect::<HashMap<_, _>>();
    let partitions = std::thread::scope(|scope| {
        let handles = roots
            .iter()
            .map(|root| {
                let executable_names = &executable_names;
                let descriptors = &descriptors;
                scope
                    .spawn(move || discover_disk_index_in_root(root, executable_names, descriptors))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect::<Vec<_>>()
    });
    let mut executables = HashMap::<String, Vec<PathBuf>>::new();
    let mut executable_keys = HashMap::<String, HashSet<String>>::new();
    let mut manifests = Vec::new();
    let mut manifest_keys = HashSet::new();

    for partition in partitions {
        for (tool_id, paths) in partition.executables {
            let destination = executables.entry(tool_id.clone()).or_default();
            let seen = executable_keys.entry(tool_id).or_default();
            destination.extend(
                paths
                    .into_iter()
                    .filter(|path| seen.insert(normalized_path_key(path))),
            );
        }
        for manifest in partition.manifests {
            if manifest_keys.insert(normalized_path_key(&manifest.installation_path)) {
                manifests.push(manifest);
            }
        }
    }

    DiskDiscoveryIndex {
        schema_version: DISCOVERY_INDEX_SCHEMA,
        scanned_at: Utc::now(),
        executables,
        manifests,
    }
}

fn discover_disk_index_in_root(
    root: &Path,
    executable_names: &HashMap<String, String>,
    descriptors: &HashMap<&str, &ToolDescriptor>,
) -> DiskDiscoveryIndex {
    let mut executables = HashMap::<String, Vec<PathBuf>>::new();
    let mut executable_keys = HashMap::<String, HashSet<String>>::new();
    let mut manifests = Vec::new();
    let mut manifest_keys = HashSet::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true)
        .into_iter()
        .filter_entry(|entry| should_scan_disk_entry(root, entry.path()))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name == ".envpilot-install.json" {
            if let Some(manifest) = read_discovered_manifest(entry.path()) {
                let key = normalized_path_key(&manifest.installation_path);
                if manifest_keys.insert(key) {
                    manifests.push(manifest);
                }
            }
            continue;
        }
        let Some(tool_id) = executable_names.get(&name) else {
            continue;
        };
        let Some(descriptor) = descriptors.get(tool_id.as_str()) else {
            continue;
        };
        if !supported_disk_candidate(descriptor, entry.path()) {
            continue;
        }
        let path = canonical(entry.path()).unwrap_or_else(|| entry.path().to_path_buf());
        let key = normalized_path_key(&path);
        if executable_keys
            .entry(tool_id.clone())
            .or_default()
            .insert(key)
        {
            executables.entry(tool_id.clone()).or_default().push(path);
        }
    }

    DiskDiscoveryIndex {
        schema_version: DISCOVERY_INDEX_SCHEMA,
        scanned_at: Utc::now(),
        executables,
        manifests,
    }
}

fn should_scan_disk_entry(root: &Path, path: &Path) -> bool {
    if path == root {
        return true;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(
        name.as_str(),
        ".git"
            | ".svn"
            | ".hg"
            | "node_modules"
            | "target"
            | ".venv"
            | "venv"
            | ".tox"
            | ".cache"
            | ".pytest_cache"
            | "__pycache__"
            | "$recycle.bin"
            | "system volume information"
            | "windowsapps"
            | "package cache"
            | "temp"
            | "tmp"
    ) {
        return false;
    }
    let normalized = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    if [
        r"\.cargo\registry",
        r"\.cargo\git",
        r"\.gradle\caches",
        r"\.m2\repository",
        r"\.nuget\packages",
        r"\.npm\_cacache",
        r"\npm-cache",
        r"\.pnpm-store",
        r"\appdata\local\packages",
        r"\appdata\local\pip\cache",
        r"\programdata\package cache",
    ]
    .iter()
    .any(|fragment| normalized.contains(fragment))
    {
        return false;
    }
    let first_component = path
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase());
    !matches!(
        first_component.as_deref(),
        Some("windows" | "recovery" | "$winreagent")
    )
}

fn supported_disk_candidate(descriptor: &ToolDescriptor, executable: &Path) -> bool {
    if descriptor.id == "python"
        && executable
            .parent()
            .is_some_and(|parent| parent.ends_with("Scripts"))
        && executable
            .parent()
            .and_then(Path::parent)
            .is_some_and(|root| root.join("pyvenv.cfg").is_file())
    {
        return false;
    }
    true
}

fn read_discovered_manifest(path: &Path) -> Option<InstallManifest> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > 1024 * 1024 {
        return None;
    }
    let manifest = serde_json::from_slice::<InstallManifest>(&fs::read(path).ok()?).ok()?;
    valid_discovered_manifest(path, &manifest).then_some(manifest)
}

fn merge_discovered_manifests(
    manifests: &mut Vec<InstallManifest>,
    discovered: Vec<InstallManifest>,
) {
    let mut seen = manifests
        .iter()
        .map(|manifest| normalized_path_key(&manifest.installation_path))
        .collect::<HashSet<_>>();
    manifests.extend(
        discovered
            .into_iter()
            .filter(|manifest| seen.insert(normalized_path_key(&manifest.installation_path))),
    );
}

#[cfg(windows)]
fn fixed_drive_roots() -> Vec<PathBuf> {
    let mut buffer = vec![0u16; 512];
    let length = unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) };
    if length == 0 || length as usize > buffer.len() {
        return Vec::new();
    }
    let mut roots = Vec::new();
    let mut start = 0usize;
    for index in 0..length as usize {
        if buffer[index] != 0 {
            continue;
        }
        if index > start {
            let mut wide = buffer[start..index].to_vec();
            wide.push(0);
            if unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_FIXED {
                roots.push(PathBuf::from(String::from_utf16_lossy(
                    &buffer[start..index],
                )));
            }
        }
        start = index + 1;
    }
    roots
}

#[cfg(not(windows))]
fn fixed_drive_roots() -> Vec<PathBuf> {
    Vec::new()
}

fn deduplicate_roots(roots: &mut Vec<PathBuf>) {
    let mut normalized = roots
        .drain(..)
        .filter(|root| root.is_dir())
        .map(|root| canonical(&root).unwrap_or(root))
        .collect::<Vec<_>>();
    normalized.sort_by_key(|root| root.components().count());
    let mut unique = Vec::<PathBuf>::new();
    for root in normalized {
        if unique
            .iter()
            .any(|existing| path_is_same_or_below(&root, existing))
        {
            continue;
        }
        unique.push(root);
    }
    *roots = unique;
}

fn valid_discovered_manifest(path: &Path, manifest: &InstallManifest) -> bool {
    if manifest.schema_version != 1
        || !manifest.managed_root.is_absolute()
        || !manifest.installation_path.is_absolute()
        || manifest.installation_path == manifest.managed_root
        || !manifest
            .installation_path
            .starts_with(&manifest.managed_root)
    {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    let parent = canonical(parent).unwrap_or_else(|| parent.to_path_buf());
    let installation = canonical(&manifest.installation_path)
        .unwrap_or_else(|| manifest.installation_path.clone());
    parent == installation
        && manifest_executables(manifest)
            .into_iter()
            .any(|executable| executable.is_file())
}

fn recover_tool_roots(
    registry: &PluginRegistry,
    preferences: &mut crate::model::ToolRootPreferences,
    manifests: &[InstallManifest],
) -> bool {
    let mut changed = false;
    for manifest in manifests {
        if preferences.roots.contains_key(&manifest.tool_id)
            || registry.get(&manifest.tool_id).is_err()
        {
            continue;
        }
        preferences.roots.insert(
            manifest.tool_id.clone(),
            crate::paths::simplify(manifest.managed_root.clone()),
        );
        changed = true;
    }
    changed
}

fn normalized_path_key(path: &Path) -> String {
    canonical(path)
        .unwrap_or_else(|| path.to_path_buf())
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn scan_tool(
    plugin: &dyn ToolPlugin,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    sources: ToolScanSources<'_>,
    probes: &mut ToolProbeContext,
) -> ToolInventory {
    let descriptor = plugin.descriptor();
    let mut issues = Vec::new();
    let mut active_paths = path_executables(descriptor.executable, user, system);
    let mut active_candidates = active_paths
        .into_iter()
        .map(|path| (path, "PATH".to_string()))
        .collect::<Vec<_>>();
    deduplicate_candidates(&mut active_candidates);
    active_paths = active_candidates
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    let default_executable = default_executable_from_environment(descriptor, user, system)
        .or_else(|| active_paths.first().and_then(|path| canonical(path)));
    let mut candidates = active_paths
        .iter()
        .cloned()
        .map(|path| (path, "PATH".to_string()))
        .collect::<Vec<_>>();

    add_environment_candidates(descriptor, user, system, &mut candidates);
    add_specialized_candidates(descriptor, user, system, &mut candidates);
    candidates.extend(
        sources
            .disk_executables
            .iter()
            .cloned()
            .map(|path| (path, "全机磁盘扫描".to_string())),
    );
    candidates.extend(
        sources
            .configured_executables
            .iter()
            .cloned()
            .map(|path| (path, "已配置安装目录".to_string())),
    );
    deduplicate_candidates(&mut candidates);

    let version_pattern = (descriptor.id != "android-ndk")
        .then(|| Regex::new(descriptor.version_pattern).ok())
        .flatten();
    let mut installed = Vec::new();
    for (executable, source) in candidates {
        if !executable.is_file() {
            continue;
        }
        if let Some(version) = inspect_candidate(
            descriptor,
            version_pattern.as_ref(),
            &executable,
            &source,
            &default_executable,
            probes,
        ) {
            installed.push(version);
        }
    }
    for manifest in sources
        .manifests
        .iter()
        .filter(|manifest| manifest.tool_id == descriptor.id)
    {
        for executable in manifest_executables(manifest) {
            if let Some(mut version) = inspect_candidate(
                descriptor,
                version_pattern.as_ref(),
                &executable,
                "EnvNexus AI",
                &default_executable,
                probes,
            ) {
                version.managed = true;
                version.path = crate::paths::simplify(manifest.installation_path.clone());
                installed.push(version);
                break;
            }
        }
    }
    add_virtual_installed_versions(descriptor, user, system, &mut installed);
    if descriptor.id == "android-sdk"
        && let Some(root) = sources.configured_root
    {
        add_android_sdk_platforms(root, "已配置安装目录", &mut installed);
    }
    deduplicate_installed(&mut installed);

    crate::versioning::sort_installed_versions_descending(&mut installed);
    let default_version = installed.iter().find(|version| version.is_default).cloned();

    if default_version.is_none() && !installed.is_empty() {
        issues.push(DiagnosticIssue {
            code: format!("{}_NO_DEFAULT", descriptor.id.to_ascii_uppercase()),
            level: IssueLevel::Warning,
            title: format!("{} 已安装但没有可解析的默认命令", descriptor.display_name),
            detail: "已发现版本目录，但当前 PATH 没有将其中任何一个设为默认。".to_string(),
            evidence: None,
            repairable: true,
        });
    }

    if active_paths.len() > 1 {
        let manager_sources = installed
            .iter()
            .map(|version| version.source.as_str())
            .filter(|source| {
                matches!(
                    *source,
                    "pyenv-win" | "nvm-windows" | "rustup" | "fnm" | "Volta" | "jabba" | "goenv"
                )
            })
            .collect::<HashSet<_>>();
        issues.push(DiagnosticIssue {
            code: format!("{}_PATH_SHADOWING", descriptor.id.to_ascii_uppercase()),
            level: IssueLevel::Warning,
            title: format!("{} 存在 PATH 遮蔽", descriptor.display_name),
            detail: if manager_sources.is_empty() {
                format!(
                    "当前进程可解析到 {} 个同名命令，只有第一个会作为默认版本。",
                    active_paths.len()
                )
            } else {
                format!(
                    "当前进程可解析到 {} 个同名命令；已识别版本管理器 {}，修复时必须保留其 shim/链接目录。",
                    active_paths.len(),
                    manager_sources.into_iter().collect::<Vec<_>>().join("、")
                )
            },
            evidence: Some(
                active_paths
                    .iter()
                    .take(5)
                    .map(|path| path.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" | "),
            ),
            repairable: true,
        });
    }

    let environment_status = if installed.is_empty() {
        HealthLevel::Unknown
    } else if issues.iter().any(|issue| issue.level == IssueLevel::Error) {
        HealthLevel::Error
    } else if issues
        .iter()
        .any(|issue| issue.level == IssueLevel::Warning)
    {
        HealthLevel::Warning
    } else {
        HealthLevel::Healthy
    };

    ToolInventory {
        id: descriptor.id.to_string(),
        display_name: descriptor.display_name.to_string(),
        category: descriptor.category.to_string(),
        icon: descriptor.icon.to_string(),
        capabilities: ToolCapabilities {
            install: plugin.supports_install(),
            switch_default: plugin.supports_switch(),
            repair: plugin.supports_repair(),
            uninstall: plugin.supports_uninstall(),
        },
        default_version,
        installed_versions: installed,
        environment_status,
        issues,
        scanned_at: Utc::now(),
    }
}

fn default_executable_from_environment(
    descriptor: &ToolDescriptor,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
) -> Option<PathBuf> {
    [system, user]
        .into_iter()
        .flat_map(|environment| split_path(get_case_insensitive(environment, "PATH")))
        .filter(|entry| !entry.is_empty() && !entry.contains('%'))
        .map(PathBuf::from)
        .map(|directory| directory.join(descriptor.executable))
        .find(|executable| executable.is_file())
        .and_then(|executable| canonical(&executable))
}

fn detect_version_managers(
    user: &EnvironmentMap,
    system: &EnvironmentMap,
) -> Vec<VersionManagerInventory> {
    struct ManagerSpec {
        id: &'static str,
        display_name: &'static str,
        tool_ids: &'static [&'static str],
        executables: &'static [&'static str],
        root_names: &'static [&'static str],
        current_args: &'static [&'static str],
    }
    let specs = [
        ManagerSpec {
            id: "pyenv-win",
            display_name: "pyenv-win",
            tool_ids: &["python"],
            executables: &["pyenv.exe", "pyenv.bat"],
            root_names: &["PYENV_ROOT", "PYENV"],
            current_args: &["version-name"],
        },
        ManagerSpec {
            id: "nvm-windows",
            display_name: "NVM for Windows",
            tool_ids: &["node"],
            executables: &["nvm.exe"],
            root_names: &["NVM_HOME"],
            current_args: &["current"],
        },
        ManagerSpec {
            id: "fnm",
            display_name: "Fast Node Manager",
            tool_ids: &["node"],
            executables: &["fnm.exe"],
            root_names: &["FNM_DIR", "FNM_MULTISHELL_PATH"],
            current_args: &["current"],
        },
        ManagerSpec {
            id: "volta",
            display_name: "Volta",
            tool_ids: &["node"],
            executables: &["volta.exe"],
            root_names: &["VOLTA_HOME"],
            current_args: &["which", "node"],
        },
        ManagerSpec {
            id: "rustup",
            display_name: "rustup",
            tool_ids: &["rust"],
            executables: &["rustup.exe"],
            root_names: &["RUSTUP_HOME"],
            current_args: &["show", "active-toolchain"],
        },
        ManagerSpec {
            id: "jabba",
            display_name: "Jabba",
            tool_ids: &["java"],
            executables: &["jabba.exe"],
            root_names: &["JABBA_HOME"],
            current_args: &["current"],
        },
        ManagerSpec {
            id: "goenv",
            display_name: "goenv",
            tool_ids: &["go"],
            executables: &["goenv.exe", "goenv.bat"],
            root_names: &["GOENV_ROOT"],
            current_args: &["version-name"],
        },
        ManagerSpec {
            id: "rbenv",
            display_name: "rbenv",
            tool_ids: &["ruby"],
            executables: &["rbenv.exe", "rbenv.bat"],
            root_names: &["RBENV_ROOT"],
            current_args: &["version-name"],
        },
        ManagerSpec {
            id: "uru",
            display_name: "Uru",
            tool_ids: &["ruby"],
            executables: &["uru_rt.exe", "uru.exe"],
            root_names: &["URU_HOME"],
            current_args: &["ls"],
        },
    ];

    specs
        .into_iter()
        .filter_map(|spec| {
            let executable = spec
                .executables
                .iter()
                .find_map(|name| path_executables(name, user, system).into_iter().next());
            let root = collect_candidate_roots(user, system, spec.root_names)
                .into_iter()
                .next();
            if executable.is_none() && root.is_none() {
                return None;
            }
            let current_version = executable.as_ref().and_then(|path| {
                let output = run_capture(path, spec.current_args, Duration::from_secs(4)).ok()?;
                output_text(&output)
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(|line| line.trim_end_matches(" (default)").to_string())
            });
            let evidence = match (&executable, &root) {
                (Some(executable), Some(root)) => {
                    format!("命令={} | 根目录={}", executable.display(), root.display())
                }
                (Some(executable), None) => format!("命令={}", executable.display()),
                (None, Some(root)) => format!("根目录={}", root.display()),
                (None, None) => unreachable!(),
            };
            Some(VersionManagerInventory {
                id: spec.id.to_string(),
                display_name: spec.display_name.to_string(),
                tool_ids: spec
                    .tool_ids
                    .iter()
                    .map(|tool| (*tool).to_string())
                    .collect(),
                executable,
                root,
                current_version,
                evidence,
            })
        })
        .collect()
}

fn deduplicate_installed(installed: &mut Vec<InstalledVersion>) {
    let mut positions = HashMap::<String, usize>::new();
    let mut unique = Vec::<InstalledVersion>::new();
    for version in installed.drain(..) {
        let key = normalized_path_key(&version.path);
        if let Some(index) = positions.get(&key).copied() {
            if version.managed || (!unique[index].is_default && version.is_default) {
                unique[index] = version;
            }
        } else {
            positions.insert(key, unique.len());
            unique.push(version);
        }
    }
    *installed = unique;
}

fn inspect_candidate(
    descriptor: &ToolDescriptor,
    version_pattern: Option<&Regex>,
    executable: &Path,
    source: &str,
    default_executable: &Option<PathBuf>,
    probes: &mut ToolProbeContext,
) -> Option<InstalledVersion> {
    let executable =
        canonical(executable).unwrap_or_else(|| crate::paths::simplify(executable.to_path_buf()));
    let root = crate::paths::simplify(installation_root_for(descriptor, &executable));
    let fingerprint = tool_probe_fingerprint(descriptor, &executable, &root)?;
    let cache_key = format!("{}|{}", descriptor.id, normalized_path_key(&executable));
    probes.touched_keys.insert(cache_key.clone());
    let is_default = Some(&executable) == default_executable.as_ref();
    if probes.reuse_cached
        && cacheable_probe_candidate(source, &executable)
        && let Some(cached) = probes.cache.entries.get(&cache_key)
        && cached.tool_id == descriptor.id
        && cached.fingerprint == fingerprint
    {
        return Some(InstalledVersion {
            version: cached.version.clone(),
            path: cached.installation_root.clone(),
            source: source.to_string(),
            is_default,
            managed: false,
            health: cached.health,
            executable: Some(executable),
        });
    }

    let output = run_capture(&executable, descriptor.version_args, Duration::from_secs(4)).ok()?;
    let text = output_text(&output);
    let version = if descriptor.id == "android-ndk" {
        package_revision(&root)?
    } else {
        version_pattern?
            .captures(&text)
            .and_then(|captures| captures.get(1))
            .map(|capture| capture.as_str().trim().to_string())?
    };
    let health = if output.status.success() {
        HealthLevel::Healthy
    } else {
        HealthLevel::Warning
    };
    probes.cache.entries.insert(
        cache_key,
        ToolProbeCacheEntry {
            tool_id: descriptor.id.to_string(),
            executable: executable.clone(),
            fingerprint,
            version: version.clone(),
            installation_root: root.clone(),
            health,
        },
    );
    Some(InstalledVersion {
        version,
        path: root,
        source: source.to_string(),
        is_default,
        managed: false,
        health,
        executable: Some(executable),
    })
}

fn cacheable_probe_candidate(source: &str, executable: &Path) -> bool {
    if source == "PATH" {
        return false;
    }
    !executable.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("shims")
    })
}

fn tool_probe_fingerprint(
    descriptor: &ToolDescriptor,
    executable: &Path,
    installation_root: &Path,
) -> Option<ToolProbeFingerprint> {
    let (executable_size, executable_modified_millis) = file_fingerprint(executable)?;
    let companion = (descriptor.id == "android-ndk")
        .then(|| file_fingerprint(&installation_root.join("source.properties")))
        .flatten();
    Some(ToolProbeFingerprint {
        executable_size,
        executable_modified_millis,
        companion_size: companion.map(|value| value.0),
        companion_modified_millis: companion.map(|value| value.1),
    })
}

fn file_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    let modified_millis = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()?;
    Some((metadata.len(), modified_millis))
}

fn installation_root_for(descriptor: &ToolDescriptor, executable: &Path) -> PathBuf {
    if descriptor.id == "android-ndk"
        && let Some(build) = executable.parent()
        && build
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("build"))
        && let Some(root) = build.parent()
        && root.join("source.properties").is_file()
    {
        return root.to_path_buf();
    }
    if descriptor.id == "android-sdk"
        && let Some(bin) = executable.parent()
        && let Some(tools) = bin.parent()
        && tools
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("tools"))
    {
        return tools.parent().unwrap_or(tools).to_path_buf();
    }
    installation_root(executable, descriptor.path_depth)
}

fn package_revision(root: &Path) -> Option<String> {
    fs::read_to_string(root.join("source.properties"))
        .ok()?
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("Pkg.Revision")
                .then(|| value.trim().to_string())
        })
}

fn path_executables(
    executable: &str,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
) -> Vec<PathBuf> {
    let mut paths = [system, user]
        .into_iter()
        .flat_map(|environment| split_path(get_case_insensitive(environment, "PATH")))
        .filter(|entry| !entry.is_empty() && !entry.contains('%'))
        .map(PathBuf::from)
        .map(|directory| directory.join(executable))
        .filter(|candidate| candidate.is_file())
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(normalized_path_key(path)));
    paths
}

fn add_environment_candidates(
    descriptor: &ToolDescriptor,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    candidates: &mut Vec<(PathBuf, String)>,
) {
    let roots = collect_candidate_roots(user, system, descriptor.home_variables);
    for root in roots {
        for executable in candidate_executables(descriptor, &root) {
            candidates.push((executable, "环境变量".to_string()));
        }
    }

    for (scope, map) in [("用户 PATH", user), ("系统 PATH", system)] {
        for entry in split_path(get_case_insensitive(map, "PATH")) {
            if entry.is_empty() || entry.contains('%') {
                continue;
            }
            let executable = PathBuf::from(entry).join(descriptor.executable);
            if executable.is_file() {
                candidates.push((executable, scope.to_string()));
            }
        }
    }
}

fn candidate_executables(descriptor: &ToolDescriptor, root: &Path) -> Vec<PathBuf> {
    match descriptor.id {
        "java" | "go" | "gradle" | "cmake" | "maven" | "ruby" => vec![
            root.join("bin").join(descriptor.executable),
            root.join(descriptor.executable),
        ],
        "python" => vec![
            root.join(descriptor.executable),
            root.join("shims").join(descriptor.executable),
        ],
        "rust" => vec![
            root.join("bin").join(descriptor.executable),
            root.join(descriptor.executable),
        ],
        "node" => vec![
            root.join(descriptor.executable),
            root.join("nodejs").join(descriptor.executable),
        ],
        "android-sdk" => vec![
            root.join("cmdline-tools")
                .join("latest")
                .join("bin")
                .join(descriptor.executable),
            root.join("tools").join("bin").join(descriptor.executable),
        ],
        "adb" => vec![root.join("platform-tools").join(descriptor.executable)],
        "android-ndk" => vec![root.join(descriptor.executable)],
        _ => vec![root.join(descriptor.executable)],
    }
}

fn add_specialized_candidates(
    descriptor: &ToolDescriptor,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    candidates: &mut Vec<(PathBuf, String)>,
) {
    match descriptor.id {
        "node" => {
            for root in collect_candidate_roots(user, system, &["NVM_HOME"]) {
                add_version_directories(&root, "v", "node.exe", "nvm-windows", candidates);
            }
        }
        "python" => {
            for root in collect_candidate_roots(user, system, &["PYENV_ROOT", "PYENV"]) {
                add_version_directories(
                    &root.join("versions"),
                    "",
                    "python.exe",
                    "pyenv-win",
                    candidates,
                );
            }
            add_python_launcher_candidates(candidates);
        }
        "java" => {
            for root in collect_candidate_roots(user, system, &["JAVA_HOME"]) {
                if let Some(parent) = root.parent() {
                    add_version_directories(parent, "", r"bin\java.exe", "JDK 目录", candidates);
                }
            }
        }
        "rust" => add_rustup_candidates(user, system, candidates),
        "android-ndk" => {
            for sdk_root in android_sdk_roots(user, system) {
                add_version_directories(
                    &sdk_root.join("ndk"),
                    "",
                    "ndk-build.cmd",
                    "Android SDK",
                    candidates,
                );
                let legacy = sdk_root.join("ndk-bundle").join("ndk-build.cmd");
                if legacy.is_file() {
                    candidates.push((legacy, "Android SDK".to_string()));
                }
            }
        }
        "cmake" => {
            for sdk_root in android_sdk_roots(user, system) {
                add_version_directories(
                    &sdk_root.join("cmake"),
                    "",
                    r"bin\cmake.exe",
                    "Android SDK",
                    candidates,
                );
            }
        }
        _ => {}
    }
}

fn add_virtual_installed_versions(
    descriptor: &ToolDescriptor,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    installed: &mut Vec<InstalledVersion>,
) {
    if descriptor.id != "android-sdk" {
        return;
    }
    for root in android_sdk_roots(user, system) {
        add_android_sdk_platforms(&root, "Android SDK Platforms", installed);
    }
}

fn add_android_sdk_platforms(root: &Path, source: &str, installed: &mut Vec<InstalledVersion>) {
    let platforms = root.join("platforms");
    let Ok(entries) = fs::read_dir(platforms) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(api) = name.strip_prefix("android-") else {
            continue;
        };
        installed.push(InstalledVersion {
            version: format!("API {api}"),
            path: entry.path(),
            source: source.to_string(),
            is_default: false,
            managed: false,
            health: if entry.path().join("android.jar").is_file() {
                HealthLevel::Healthy
            } else {
                HealthLevel::Warning
            },
            executable: None,
        });
    }
}

fn add_version_directories(
    root: &Path,
    prefix: &str,
    relative_executable: &str,
    source: &str,
    candidates: &mut Vec<(PathBuf, String)>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir()
            || !entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
        {
            continue;
        }
        let executable = entry.path().join(relative_executable);
        if executable.is_file() {
            candidates.push((executable, source.to_string()));
        }
    }
}

fn add_python_launcher_candidates(candidates: &mut Vec<(PathBuf, String)>) {
    let launcher = Path::new(r"C:\Windows\py.exe");
    let Ok(output) = run_capture(launcher, &["-0p"], Duration::from_secs(4)) else {
        return;
    };
    for line in output_text(&output).lines() {
        let trimmed = line.trim().trim_start_matches(['-', 'V', ':', '*', ' ']);
        if let Some(index) = trimmed.to_ascii_lowercase().find(r":\") {
            let drive_index = index.saturating_sub(1);
            let path = PathBuf::from(&trimmed[drive_index..].trim());
            if path.is_file() {
                candidates.push((path, "Python Launcher".to_string()));
            }
        }
    }
}

fn add_rustup_candidates(
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    candidates: &mut Vec<(PathBuf, String)>,
) {
    let rustup = path_executables("rustup.exe", user, system)
        .into_iter()
        .next();
    let Some(rustup) = rustup else {
        return;
    };
    let Ok(output) = run_capture(&rustup, &["toolchain", "list"], Duration::from_secs(4)) else {
        return;
    };
    for line in output_text(&output).lines() {
        let toolchain = line.split_whitespace().next().unwrap_or_default();
        if toolchain.is_empty() {
            continue;
        }
        let Ok(which) = run_capture(
            &rustup,
            &["which", "--toolchain", toolchain, "rustc"],
            Duration::from_secs(4),
        ) else {
            continue;
        };
        let path = output_text(&which);
        let path = PathBuf::from(path.lines().next().unwrap_or_default().trim());
        if path.is_file() {
            candidates.push((path, "rustup".to_string()));
        }
    }
}

fn android_sdk_roots(user: &EnvironmentMap, system: &EnvironmentMap) -> Vec<PathBuf> {
    let mut roots = collect_candidate_roots(user, system, &["ANDROID_HOME", "ANDROID_SDK_ROOT"]);
    for adb in path_executables("adb.exe", user, system) {
        if let Some(root) = adb.parent().and_then(Path::parent) {
            roots.push(root.to_path_buf());
        }
    }
    roots
}

fn deduplicate_candidates(candidates: &mut Vec<(PathBuf, String)>) {
    let mut seen = HashSet::new();
    candidates.retain(|(path, _)| {
        let key = canonical(path)
            .unwrap_or_else(|| path.to_path_buf())
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
        seen.insert(key)
    });
}

fn installation_root(executable: &Path, depth: usize) -> PathBuf {
    let mut root = executable.to_path_buf();
    for _ in 0..=depth {
        let Some(parent) = root.parent() else {
            break;
        };
        root = parent.to_path_buf();
    }
    root
}

fn canonical(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path).ok().map(crate::paths::simplify)
}

fn add_cross_tool_diagnostics(
    tools: &[ToolInventory],
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    issues: &mut Vec<DiagnosticIssue>,
) {
    let by_id = tools
        .iter()
        .map(|tool| (tool.id.as_str(), tool))
        .collect::<HashMap<_, _>>();
    if let (Some(java), Some(java_home)) = (
        by_id
            .get("java")
            .and_then(|tool| tool.default_version.as_ref()),
        get_case_insensitive(user, "JAVA_HOME")
            .or_else(|| get_case_insensitive(system, "JAVA_HOME")),
    ) {
        let home = PathBuf::from(java_home);
        if canonical(&home) != canonical(&java.path) {
            issues.push(DiagnosticIssue {
                code: "JAVA_HOME_DEFAULT_MISMATCH".to_string(),
                level: IssueLevel::Warning,
                title: "JAVA_HOME 与默认 java 命令不一致".to_string(),
                detail: "构建工具读取 JAVA_HOME 时可能得到与终端 java 命令不同的 JDK。".to_string(),
                evidence: Some(format!(
                    "JAVA_HOME={} | java={}",
                    home.display(),
                    java.path.display()
                )),
                repairable: true,
            });
        }
    }

    if let Some(android) = by_id
        .get("adb")
        .and_then(|tool| tool.default_version.as_ref())
    {
        if let Some(java) = by_id
            .get("java")
            .and_then(|tool| tool.default_version.as_ref())
            && drive_key(&android.path) != drive_key(&java.path)
        {
            issues.push(DiagnosticIssue {
                code: "ANDROID_WORKSPACE_DISTRIBUTED".to_string(),
                level: IssueLevel::Info,
                title: "Android 依赖当前分布在不同根目录".to_string(),
                detail: "EnvNexus AI 可以为后续安装建立统一根目录，但不会自动迁移已有目录。"
                    .to_string(),
                evidence: Some(format!(
                    "ADB={} | JDK={}",
                    android.path.display(),
                    java.path.display()
                )),
                repairable: false,
            });
        }
    }
}

fn add_version_manager_diagnostics(
    tools: &[ToolInventory],
    managers: &[VersionManagerInventory],
    issues: &mut Vec<DiagnosticIssue>,
) {
    let mut managers_by_tool = HashMap::<&str, Vec<&VersionManagerInventory>>::new();
    for manager in managers {
        for tool_id in &manager.tool_ids {
            managers_by_tool
                .entry(tool_id.as_str())
                .or_default()
                .push(manager);
        }
    }
    for (tool_id, tool_managers) in managers_by_tool {
        if tool_managers.len() > 1 {
            issues.push(DiagnosticIssue {
                code: format!("MULTIPLE_VERSION_MANAGERS_{}", tool_id.to_ascii_uppercase()),
                level: IssueLevel::Error,
                title: format!("{tool_id} 同时存在多个版本管理器"),
                detail:
                    "多个管理器可能同时注入 shim、符号链接或 shell 初始化脚本，CMD、PowerShell 与 IDE 终端可能解析到不同版本。"
                        .to_string(),
                evidence: Some(
                    tool_managers
                        .iter()
                        .map(|manager| {
                            format!("{}: {}", manager.display_name, manager.evidence)
                        })
                        .collect::<Vec<_>>()
                        .join(" | "),
                ),
                repairable: false,
            });
        }

        let Some(tool) = tools.iter().find(|tool| tool.id == tool_id) else {
            continue;
        };
        let manager_roots = tool_managers
            .iter()
            .filter_map(|manager| manager.root.as_deref())
            .collect::<Vec<_>>();
        if manager_roots.is_empty() {
            continue;
        }
        let external = tool
            .installed_versions
            .iter()
            .filter(|version| {
                !manager_roots
                    .iter()
                    .any(|root| path_is_same_or_below(&version.path, root))
            })
            .take(5)
            .collect::<Vec<_>>();
        if !external.is_empty() {
            issues.push(DiagnosticIssue {
                code: format!(
                    "VERSION_MANAGER_EXTERNAL_INSTALLS_{}",
                    tool_id.to_ascii_uppercase()
                ),
                level: IssueLevel::Info,
                title: format!(
                    "{} 版本管理器与外部安装同时存在",
                    tool_managers
                        .iter()
                        .map(|manager| manager.display_name.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
                detail:
                    "这可以正常共存，但修复 PATH 时必须保留管理器 shim，并明确外部安装是否仍需作为回退版本。"
                        .to_string(),
                evidence: Some(
                    external
                        .iter()
                        .map(|version| {
                            format!("{}: {}", version.version, version.path.display())
                        })
                        .collect::<Vec<_>>()
                        .join(" | "),
                ),
                repairable: false,
            });
        }
    }
}

fn path_is_same_or_below(path: &Path, root: &Path) -> bool {
    let path = canonical(path).unwrap_or_else(|| path.to_path_buf());
    let root = canonical(root).unwrap_or_else(|| root.to_path_buf());
    let path = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let root = root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase();
    path == root || path.starts_with(&format!("{root}\\"))
}

fn drive_key(path: &Path) -> Option<String> {
    let key = path
        .to_string_lossy()
        .chars()
        .take(2)
        .collect::<String>()
        .to_ascii_lowercase();
    (key.len() == 2).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_up_from_executable_to_installation_root() {
        let java = Path::new(r"E:\Java\jdk-21\bin\java.exe");
        assert_eq!(installation_root(java, 1), PathBuf::from(r"E:\Java\jdk-21"));
    }

    #[test]
    fn android_ndk_build_wrapper_maps_to_the_ndk_root_and_package_revision() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("ndk").join("27.3.13750724");
        let executable = root.join("build").join("ndk-build.cmd");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"").unwrap();
        fs::write(
            root.join("source.properties"),
            b"Pkg.Desc = Android NDK\nPkg.Revision = 27.3.13750724\n",
        )
        .unwrap();
        let registry = PluginRegistry::builtin();
        let ndk = registry.get("android-ndk").unwrap();

        assert_eq!(installation_root_for(ndk.descriptor(), &executable), root);
        assert_eq!(
            package_revision(&installation_root_for(ndk.descriptor(), &executable)).as_deref(),
            Some("27.3.13750724")
        );
    }

    #[test]
    fn deduplicates_paths_case_insensitively() {
        let mut values = vec![
            (PathBuf::from(r"E:\Tools\node.exe"), "a".into()),
            (PathBuf::from(r"e:\tools\NODE.exe"), "b".into()),
        ];
        deduplicate_candidates(&mut values);
        assert_eq!(values.len(), 1);
    }

    #[test]
    fn configured_root_discovers_versions_created_after_command_scripts() {
        let root = tempfile::tempdir().unwrap();
        let executable = root
            .path()
            .join("java")
            .join("21.0.7")
            .join("bin")
            .join("java.exe");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"test").unwrap();
        let registry = PluginRegistry::builtin();
        let discovery = discover_disk_index_in_roots(&registry, &[root.path().to_path_buf()]);

        assert_eq!(
            discovery.executables.get("java"),
            Some(&vec![crate::paths::simplify(
                fs::canonicalize(executable).unwrap()
            )])
        );
    }

    #[test]
    fn configured_root_discovers_python_version_directories() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("python").join("3.14.6").join("python.exe");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"test").unwrap();
        let registry = PluginRegistry::builtin();
        let discovery = discover_disk_index_in_roots(&registry, &[root.path().to_path_buf()]);

        assert_eq!(
            discovery.executables.get("python"),
            Some(&vec![crate::paths::simplify(
                fs::canonicalize(executable).unwrap()
            )])
        );
    }

    #[test]
    fn discovers_orphaned_manifests_and_recovers_the_managed_root() {
        let profile = tempfile::tempdir().unwrap();
        let managed_root = profile.path().join("Desktop").join("env");
        let installation = managed_root.join("python").join("3.14.6");
        fs::create_dir_all(&installation).unwrap();
        fs::write(installation.join("python.exe"), b"test").unwrap();
        let manifest = InstallManifest {
            schema_version: 1,
            operation_id: "recovery-test".to_string(),
            tool_id: "python".to_string(),
            version: "3.14.6".to_string(),
            installed_at: Utc::now(),
            managed_root: managed_root.clone(),
            installation_path: installation.clone(),
            source_url: "https://www.python.org/".to_string(),
            checksum_algorithm: None,
            checksum: None,
        };
        fs::write(
            installation.join(".envpilot-install.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let index = discover_disk_index_in_roots(
            &PluginRegistry::builtin(),
            &[profile.path().to_path_buf()],
        );
        let manifests = index.manifests;

        assert_eq!(manifests.len(), 1);
        let registry = PluginRegistry::builtin();
        let mut preferences = crate::model::ToolRootPreferences::default();
        assert!(recover_tool_roots(&registry, &mut preferences, &manifests));
        assert_eq!(preferences.roots.get("python"), Some(&managed_root));
    }

    #[test]
    fn fresh_registry_path_is_used_for_the_default_version() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("python").join("3.14.6");
        fs::create_dir_all(&selected).unwrap();
        fs::write(selected.join("python.exe"), b"test").unwrap();
        let user =
            EnvironmentMap::from([("Path".to_string(), selected.to_string_lossy().into_owned())]);
        let registry = PluginRegistry::builtin();
        let python = registry.get("python").unwrap();

        let resolved =
            default_executable_from_environment(python.descriptor(), &user, &EnvironmentMap::new())
                .unwrap();

        assert_eq!(
            resolved,
            crate::paths::simplify(fs::canonicalize(selected.join("python.exe")).unwrap())
        );
    }

    #[test]
    fn full_disk_discovery_classifies_tools_without_fixed_install_roots() {
        let drive = tempfile::tempdir().unwrap();
        let python = drive
            .path()
            .join("Users")
            .join("Example")
            .join("Desktop")
            .join("env")
            .join("python")
            .join("3.14.6")
            .join("python.exe");
        let java = drive
            .path()
            .join("Development")
            .join("jdk-25")
            .join("bin")
            .join("java.exe");
        let ignored_node = drive
            .path()
            .join("project")
            .join("node_modules")
            .join("node.exe");
        for executable in [&python, &java, &ignored_node] {
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(executable, b"test").unwrap();
        }
        let registry = PluginRegistry::builtin();

        let index = discover_disk_index_in_roots(&registry, &[drive.path().to_path_buf()]);

        assert_eq!(
            index.executables.get("python"),
            Some(&vec![crate::paths::simplify(
                fs::canonicalize(&python).unwrap()
            )])
        );
        assert_eq!(
            index.executables.get("java"),
            Some(&vec![crate::paths::simplify(
                fs::canonicalize(&java).unwrap()
            )])
        );
        assert!(!index.executables.contains_key("node"));
    }

    #[test]
    fn incremental_probe_cache_reuses_only_unchanged_executables() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("python.exe");
        fs::write(&executable, b"not a real executable").unwrap();
        let executable = canonical(&executable).unwrap();
        let registry = PluginRegistry::builtin();
        let python = registry.get("python").unwrap();
        let descriptor = python.descriptor();
        let root = installation_root_for(descriptor, &executable);
        let fingerprint = tool_probe_fingerprint(descriptor, &executable, &root).unwrap();
        let key = format!("{}|{}", descriptor.id, normalized_path_key(&executable));
        let mut probes = ToolProbeContext {
            cache: ToolProbeCache {
                schema_version: PROBE_CACHE_SCHEMA,
                entries: HashMap::from([(
                    key,
                    ToolProbeCacheEntry {
                        tool_id: descriptor.id.to_string(),
                        executable: executable.clone(),
                        fingerprint,
                        version: "3.14.6".to_string(),
                        installation_root: root,
                        health: HealthLevel::Healthy,
                    },
                )]),
            },
            touched_keys: HashSet::new(),
            reuse_cached: true,
        };
        let pattern = Regex::new(descriptor.version_pattern).unwrap();

        let cached = inspect_candidate(
            descriptor,
            Some(&pattern),
            &executable,
            "测试",
            &None,
            &mut probes,
        )
        .unwrap();
        assert_eq!(cached.version, "3.14.6");

        fs::write(&executable, b"changed and still not a real executable").unwrap();
        assert!(
            inspect_candidate(
                descriptor,
                Some(&pattern),
                &executable,
                "测试",
                &None,
                &mut probes,
            )
            .is_none()
        );
    }
}
