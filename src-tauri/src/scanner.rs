use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::Utc;
use regex::Regex;
use walkdir::WalkDir;

use crate::{
    environment::{
        EnvironmentMap, collect_candidate_roots, diagnose_environment, get_case_insensitive,
        read_environment, split_path,
    },
    error::AppResult,
    installer::{managed_manifests, manifest_executables},
    model::{
        DiagnosticIssue, EnvironmentScan, EnvironmentScope, HealthLevel, InstalledVersion,
        IssueLevel, ToolCapabilities, ToolInventory, VersionManagerInventory,
    },
    plugins::{PluginRegistry, ToolDescriptor, ToolPlugin},
    process::{output_text, run_capture},
};

pub fn scan(registry: &PluginRegistry, data_root: &Path) -> AppResult<EnvironmentScan> {
    let started = Utc::now();
    let user = read_environment(EnvironmentScope::User)?;
    let system = read_environment(EnvironmentScope::System)?;
    let mut global_issues = diagnose_environment(&user, &system);
    let mut tools = Vec::with_capacity(registry.all().len());
    let manifests = managed_manifests(data_root);
    let version_managers = detect_version_managers(&user, &system);
    let preferences = crate::read_tool_root_preferences(data_root)?;

    for plugin in registry.all() {
        let configured_root = preferences
            .roots
            .get(plugin.descriptor().id)
            .map(PathBuf::as_path);
        tools.push(scan_tool(
            plugin.as_ref(),
            &user,
            &system,
            &manifests,
            configured_root,
        ));
    }
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

fn scan_tool(
    plugin: &dyn ToolPlugin,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    manifests: &[crate::installer::InstallManifest],
    configured_root: Option<&Path>,
) -> ToolInventory {
    let descriptor = plugin.descriptor();
    let mut issues = Vec::new();
    let mut active_paths = where_paths(descriptor.executable);
    if descriptor.id == "python" {
        active_paths.extend(where_paths("python"));
        let mut paths = active_paths
            .into_iter()
            .map(|path| (path, "PATH".to_string()))
            .collect::<Vec<_>>();
        deduplicate_candidates(&mut paths);
        active_paths = paths.into_iter().map(|(path, _)| path).collect();
    }
    let default_executable = active_paths.first().and_then(|path| canonical(path));
    let mut candidates = active_paths
        .iter()
        .cloned()
        .map(|path| (path, "PATH".to_string()))
        .collect::<Vec<_>>();

    add_environment_candidates(descriptor, user, system, &mut candidates);
    add_specialized_candidates(descriptor, user, system, &mut candidates);
    if let Some(root) = configured_root {
        add_configured_root_candidates(descriptor, root, &mut candidates);
    }
    deduplicate_candidates(&mut candidates);

    let mut installed = Vec::new();
    for (executable, source) in candidates {
        if !executable.is_file() {
            continue;
        }
        if let Some(version) =
            inspect_candidate(descriptor, &executable, &source, &default_executable)
        {
            installed.push(version);
        }
    }
    for manifest in manifests
        .iter()
        .filter(|manifest| manifest.tool_id == descriptor.id)
    {
        for executable in manifest_executables(manifest) {
            if let Some(mut version) =
                inspect_candidate(descriptor, &executable, "EnvNexus AI", &default_executable)
            {
                version.managed = true;
                version.path = manifest.installation_path.clone();
                installed.push(version);
                break;
            }
        }
    }
    add_virtual_installed_versions(descriptor, user, system, &mut installed);
    if descriptor.id == "android-sdk"
        && let Some(root) = configured_root
    {
        add_android_sdk_platforms(root, "已配置安装目录", &mut installed);
    }
    deduplicate_installed(&mut installed);

    installed.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| right.version.cmp(&left.version))
            .then_with(|| left.path.cmp(&right.path))
    });
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
                .find_map(|name| where_paths(name).into_iter().next());
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
    let mut unique = Vec::new();
    for version in installed.drain(..) {
        let key = canonical(
            version
                .executable
                .as_deref()
                .unwrap_or(version.path.as_path()),
        )
        .unwrap_or_else(|| version.path.clone())
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
        if let Some(index) = positions.get(&key).copied() {
            if version.managed {
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
    executable: &Path,
    source: &str,
    default_executable: &Option<PathBuf>,
) -> Option<InstalledVersion> {
    let output = run_capture(executable, descriptor.version_args, Duration::from_secs(4)).ok()?;
    let text = output_text(&output);
    let pattern = Regex::new(descriptor.version_pattern).ok()?;
    let version = pattern
        .captures(&text)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().trim().to_string())?;
    let executable_canonical = canonical(executable);
    let is_default = executable_canonical.is_some() && executable_canonical == *default_executable;
    Some(InstalledVersion {
        version,
        path: installation_root(executable, descriptor.path_depth),
        source: source.to_string(),
        is_default,
        managed: false,
        health: if output.status.success() {
            HealthLevel::Healthy
        } else {
            HealthLevel::Warning
        },
        executable: Some(executable.to_path_buf()),
    })
}

fn where_paths(executable: &str) -> Vec<PathBuf> {
    let where_exe = Path::new(r"C:\Windows\System32\where.exe");
    let Ok(output) = run_capture(where_exe, &[executable], Duration::from_secs(3)) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
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

fn add_configured_root_candidates(
    descriptor: &ToolDescriptor,
    root: &Path,
    candidates: &mut Vec<(PathBuf, String)>,
) {
    if !root.is_dir() {
        return;
    }
    let executable = descriptor.executable.to_ascii_lowercase();
    let initial_count = candidates.len();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .max_depth(6)
        .into_iter()
        .filter_map(Result::ok)
        .take(20_000)
    {
        if candidates.len().saturating_sub(initial_count) >= 64 {
            break;
        }
        if entry.file_type().is_file()
            && entry.file_name().to_string_lossy().to_ascii_lowercase() == executable
        {
            candidates.push((entry.into_path(), "已配置安装目录".to_string()));
        }
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
        "rust" => add_rustup_candidates(candidates),
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

fn add_rustup_candidates(candidates: &mut Vec<(PathBuf, String)>) {
    let rustup = where_paths("rustup.exe").into_iter().next();
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
    for adb in where_paths("adb.exe") {
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
    fs::canonicalize(path).ok()
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
        let java = registry.get("java").unwrap();
        let mut candidates = Vec::new();

        add_configured_root_candidates(java.descriptor(), root.path(), &mut candidates);

        assert_eq!(candidates, vec![(executable, "已配置安装目录".to_string())]);
    }
}
