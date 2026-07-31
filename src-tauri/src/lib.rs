mod ai;
mod app_preferences;
pub mod application_update;
pub mod cli;
mod diagnostics;
mod environment;
mod error;
mod installer;
mod model;
mod paths;
mod plans;
mod plugins;
mod process;
mod scanner;
mod sources;
mod terminal;
mod versioning;

use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use installer::{Installer, OperationResult};
use model::{
    AiDiagnosticAnalysis, AiModelInfo, AiProviderInput, AiSettings, AppLanguage, AppPreferences,
    BootstrapState, CloseBehavior, DiagnosticGuidance, DiagnosticReport, EnvironmentBackupSummary,
    EnvironmentScan, EnvironmentScope, OperationLogEntry, OperationPlan, PlannedAction,
    TerminalCommandStatus, ToolCapabilities, ToolDefinition, ToolRootPreferences, VersionCatalog,
};
use plans::PlanService;
use plugins::PluginRegistry;
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, Runtime, State, Window, WindowEvent,
    image::Image,
    menu::{IconMenuItem, Menu, PredefinedMenuItem, Submenu},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
};

const TRAY_ID: &str = "envnexus-ai-tray";

#[derive(Debug, Clone)]
enum TrayCommand {
    OpenTool(String),
    SelectAiProvider(String),
    PreviewSwitch {
        tool_id: String,
        installation_path: PathBuf,
    },
    OpenDiagnostic(String),
    PreviewDiagnosticRepair(String),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum TrayFrontendAction {
    Navigate {
        view: String,
    },
    Scan,
    OpenTool {
        tool_id: String,
    },
    PreviewSwitch {
        tool_id: String,
        installation_path: PathBuf,
    },
    OpenDiagnostic {
        issue_code: String,
    },
    PreviewDiagnosticRepair {
        issue_code: String,
    },
    SelectAiProvider {
        provider_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrayMenuStatus {
    tool_entries: usize,
    switch_entries: usize,
    diagnostic_entries: usize,
    diagnostic_repair_entries: usize,
    ai_provider_entries: usize,
    active_ai_provider_id: Option<String>,
    language: AppLanguage,
    ready: bool,
}

struct AppState {
    registry: PluginRegistry,
    client: reqwest::Client,
    ai_client: reqwest::Client,
    update_client: reqwest::Client,
    plans: PlanService,
    installer: Installer,
    data_root: PathBuf,
    app_preferences: RwLock<AppPreferences>,
    tray_actions: RwLock<HashMap<String, TrayCommand>>,
}

const ANDROID_WORKSPACE_TOOL_IDS: [&str; 6] = [
    "android-sdk",
    "android-ndk",
    "java",
    "gradle",
    "cmake",
    "adb",
];

#[tauri::command]
fn bootstrap(state: State<'_, Arc<AppState>>) -> BootstrapState {
    BootstrapState {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        data_root: state.data_root.clone(),
        config_ready: state.data_root.is_dir(),
        platform: std::env::consts::OS.to_string(),
        installation_kind: application_update::detect_install_kind()
            .as_str()
            .to_string(),
    }
}

#[tauri::command]
async fn prepare_application_update(
    request: application_update::PrepareApplicationUpdateRequest,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<application_update::PreparedApplicationUpdate, String> {
    application_update::prepare(&state.update_client, &state.data_root, request, &app)
        .await
        .map_err(error::command_error)
}

#[tauri::command]
fn launch_application_update(
    operation_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    application_update::launch(&state.data_root, &operation_id).map_err(error::command_error)
}

#[tauri::command]
fn confirm_application_update_started() -> Result<bool, String> {
    application_update::confirm_new_version_started().map_err(error::command_error)
}

#[tauri::command]
fn app_preferences(state: State<'_, Arc<AppState>>) -> Result<AppPreferences, String> {
    Ok(state
        .app_preferences
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone())
}

#[tauri::command]
fn save_app_preferences(
    preferences: AppPreferences,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<AppPreferences, String> {
    let previous = state
        .app_preferences
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    app_preferences::set_launch_at_login(preferences.launch_at_login)
        .map_err(|error| format!("更新 Windows 开机自启动失败：{error}"))?;
    let saved = match app_preferences::write(&state.data_root, preferences) {
        Ok(saved) => saved,
        Err(error) => {
            let _ = app_preferences::set_launch_at_login(previous.launch_at_login);
            return Err(format!("保存应用行为设置失败：{error}"));
        }
    };
    *state
        .app_preferences
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = saved.clone();
    refresh_tray_menu(&app).map_err(|error| format!("刷新托盘菜单失败：{error}"))?;
    Ok(saved)
}

#[tauri::command]
fn hide_to_tray(window: tauri::WebviewWindow) -> Result<(), String> {
    window
        .hide()
        .map_err(|error| format!("最小化到托盘失败：{error}"))?;
    set_webview_low_memory_target(window.app_handle(), true);
    Ok(())
}

#[tauri::command]
fn restore_main_window(window: tauri::WebviewWindow) -> Result<(), String> {
    set_webview_low_memory_target(window.app_handle(), false);
    window
        .unminimize()
        .map_err(|error| format!("恢复 EnvNexus AI 窗口失败：{error}"))?;
    window
        .show()
        .map_err(|error| format!("显示 EnvNexus AI 窗口失败：{error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("聚焦 EnvNexus AI 窗口失败：{error}"))
}

#[tauri::command]
fn tray_ready(app: AppHandle) -> bool {
    app.tray_by_id(TRAY_ID).is_some()
}

#[tauri::command]
fn tray_menu_status(app: AppHandle, state: State<'_, Arc<AppState>>) -> TrayMenuStatus {
    let actions = state
        .tray_actions
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    TrayMenuStatus {
        tool_entries: actions
            .values()
            .filter(|action| matches!(action, TrayCommand::OpenTool(_)))
            .count(),
        switch_entries: actions
            .values()
            .filter(|action| matches!(action, TrayCommand::PreviewSwitch { .. }))
            .count(),
        diagnostic_entries: actions
            .values()
            .filter(|action| matches!(action, TrayCommand::OpenDiagnostic(_)))
            .count(),
        diagnostic_repair_entries: actions
            .values()
            .filter(|action| matches!(action, TrayCommand::PreviewDiagnosticRepair(_)))
            .count(),
        ai_provider_entries: actions
            .values()
            .filter(|action| matches!(action, TrayCommand::SelectAiProvider(_)))
            .count(),
        active_ai_provider_id: ai::read_settings(&state.data_root)
            .ok()
            .and_then(|settings| settings.active_provider_id),
        language: state
            .app_preferences
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .language,
        ready: app.tray_by_id(TRAY_ID).is_some(),
    }
}

#[tauri::command]
fn tool_definitions(state: State<'_, Arc<AppState>>) -> Vec<ToolDefinition> {
    state
        .registry
        .all()
        .iter()
        .map(|plugin| {
            let descriptor = plugin.descriptor();
            ToolDefinition {
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
            }
        })
        .collect()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataRootPointer {
    schema_version: u32,
    data_root: PathBuf,
}

#[tauri::command]
fn configure_data_root(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err("数据目录必须是绝对路径，且不能是磁盘根目录".to_string());
    }
    fs::create_dir_all(&path).map_err(|error| format!("无法创建数据目录：{error}"))?;
    let path = paths::canonicalize_simplified(&path)
        .map_err(|error| format!("无法解析数据目录：{error}"))?;
    ensure_data_layout(&path).map_err(|error| format!("无法初始化数据目录：{error}"))?;
    let pointer_path =
        data_root_pointer_path().ok_or_else(|| "无法定位 EnvNexus AI 安装目录".to_string())?;
    let pointer = DataRootPointer {
        schema_version: 1,
        data_root: path.clone(),
    };
    write_pointer_atomic(&pointer_path, &pointer)
        .map_err(|error| format!("无法保存数据目录设置：{error}"))?;
    Ok(path)
}

#[tauri::command]
fn tool_root_preferences(state: State<'_, Arc<AppState>>) -> Result<ToolRootPreferences, String> {
    read_tool_root_preferences(&state.data_root)
        .map_err(|error| format!("读取工具安装目录设置失败：{error}"))
}

#[tauri::command]
fn set_tool_root(
    tool_id: String,
    path: PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<ToolRootPreferences, String> {
    state.registry.get(&tool_id).map_err(error::command_error)?;
    let path = normalize_install_root(path)?;
    let mut preferences = read_tool_root_preferences(&state.data_root)
        .map_err(|error| format!("读取工具安装目录设置失败：{error}"))?;
    if ANDROID_WORKSPACE_TOOL_IDS.contains(&tool_id.as_str()) {
        preferences.android_root = Some(path.clone());
        for android_tool_id in ANDROID_WORKSPACE_TOOL_IDS {
            preferences
                .roots
                .insert(android_tool_id.to_string(), path.clone());
        }
    } else {
        preferences.roots.insert(tool_id, path);
    }
    write_tool_root_preferences(&state.data_root, &preferences)
        .map_err(|error| format!("保存工具安装目录设置失败：{error}"))?;
    Ok(preferences)
}

#[tauri::command]
fn set_android_root(
    path: PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<ToolRootPreferences, String> {
    let path = normalize_install_root(path)?;
    let mut preferences = read_tool_root_preferences(&state.data_root)
        .map_err(|error| format!("读取工具安装目录设置失败：{error}"))?;
    preferences.android_root = Some(path.clone());
    for tool_id in ANDROID_WORKSPACE_TOOL_IDS {
        preferences.roots.insert(tool_id.to_string(), path.clone());
    }
    write_tool_root_preferences(&state.data_root, &preferences)
        .map_err(|error| format!("保存 Android 根目录设置失败：{error}"))?;
    Ok(preferences)
}

#[tauri::command]
fn terminal_commands_status(
    state: State<'_, Arc<AppState>>,
) -> Result<TerminalCommandStatus, String> {
    terminal::status(&state.registry, &state.data_root).map_err(error::command_error)
}

#[tauri::command]
fn prepare_terminal_commands(
    state: State<'_, Arc<AppState>>,
) -> Result<TerminalCommandStatus, String> {
    terminal::prepare(&state.registry, &state.data_root).map_err(error::command_error)
}

#[tauri::command]
fn save_terminal_command_directory(
    directory: PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<TerminalCommandStatus, String> {
    terminal::save_directory(&state.registry, &state.data_root, directory)
        .map_err(error::command_error)
}

#[tauri::command]
fn preview_enable_terminal_commands(
    state: State<'_, Arc<AppState>>,
) -> Result<OperationPlan, String> {
    let status =
        terminal::prepare(&state.registry, &state.data_root).map_err(error::command_error)?;
    state
        .plans
        .preview_command_directory(status.directory, true)
        .map_err(error::command_error)
}

#[tauri::command]
fn preview_disable_terminal_commands(
    state: State<'_, Arc<AppState>>,
) -> Result<OperationPlan, String> {
    let directory = terminal::command_directory(&state.data_root).map_err(error::command_error)?;
    state
        .plans
        .preview_command_directory(directory, false)
        .map_err(error::command_error)
}

#[tauri::command]
fn ai_settings(state: State<'_, Arc<AppState>>) -> Result<AiSettings, String> {
    ai::read_settings(&state.data_root).map_err(error::command_error)
}

#[tauri::command]
fn save_ai_provider(
    input: AiProviderInput,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<AiSettings, String> {
    let settings = ai::save_provider(&state.data_root, input).map_err(error::command_error)?;
    refresh_tray_menu(&app)
        .map_err(|error| format!("AI 设置已保存，但刷新托盘菜单失败：{error}"))?;
    Ok(settings)
}

#[tauri::command]
fn clear_ai_api_key(
    provider_id: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<AiSettings, String> {
    let settings =
        ai::clear_api_key(&state.data_root, &provider_id).map_err(error::command_error)?;
    refresh_tray_menu(&app)
        .map_err(|error| format!("AI 密钥已删除，但刷新托盘菜单失败：{error}"))?;
    Ok(settings)
}

#[tauri::command]
fn select_ai_model(
    provider_id: String,
    model: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<AiSettings, String> {
    let settings =
        ai::select_model(&state.data_root, &provider_id, &model).map_err(error::command_error)?;
    refresh_tray_menu(&app)
        .map_err(|error| format!("AI 模型已保存，但刷新托盘菜单失败：{error}"))?;
    Ok(settings)
}

#[tauri::command]
fn activate_ai_provider(
    provider_id: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<AiSettings, String> {
    let settings =
        ai::activate_provider(&state.data_root, &provider_id).map_err(error::command_error)?;
    refresh_tray_menu(&app)
        .map_err(|error| format!("AI 厂商已切换，但刷新托盘菜单失败：{error}"))?;
    Ok(settings)
}

#[tauri::command]
async fn fetch_ai_models(
    provider_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<AiModelInfo>, String> {
    ai::fetch_models(&state.ai_client, &state.data_root, &provider_id)
        .await
        .map_err(error::command_error)
}

#[tauri::command]
async fn analyze_diagnostic_with_ai(
    issue_code: String,
    state: State<'_, Arc<AppState>>,
) -> Result<AiDiagnosticAnalysis, String> {
    let scan = read_cached_environment_scan(&state.data_root)
        .map_err(|error| format!("读取上次扫描快照失败：{error}"))?
        .ok_or_else(|| "尚无扫描结果，请先手动扫描".to_string())?;
    let issue = scan
        .issues
        .iter()
        .chain(scan.tools.iter().flat_map(|tool| tool.issues.iter()))
        .find(|issue| issue.code == issue_code)
        .cloned()
        .ok_or_else(|| "上次扫描快照中找不到该诊断项".to_string())?;
    let user =
        environment::read_environment(EnvironmentScope::User).map_err(error::command_error)?;
    let system =
        environment::read_environment(EnvironmentScope::System).map_err(error::command_error)?;
    let preferences = read_tool_root_preferences(&state.data_root)
        .map_err(|error| format!("读取工具目录配置失败：{error}"))?;
    let machine = diagnostics::machine_context(&state.data_root, &preferences, &user, &system);
    let guidance = diagnostics::guidance_for(&issue, &scan, &machine, &user, &system);
    ai::analyze_diagnostic(
        &state.ai_client,
        &state.data_root,
        &issue,
        &scan.version_managers,
        &machine,
        &guidance,
        &scan,
    )
    .await
    .map_err(error::command_error)
}

#[tauri::command]
fn diagnostic_guidance(
    issue_code: String,
    state: State<'_, Arc<AppState>>,
) -> Result<DiagnosticGuidance, String> {
    let scan = read_cached_environment_scan(&state.data_root)
        .map_err(|error| format!("读取上次扫描快照失败：{error}"))?
        .ok_or_else(|| "尚无扫描结果，请先手动扫描".to_string())?;
    let issue = scan
        .issues
        .iter()
        .chain(scan.tools.iter().flat_map(|tool| tool.issues.iter()))
        .find(|issue| issue.code == issue_code)
        .ok_or_else(|| "上次扫描快照中找不到该诊断项".to_string())?;
    let user =
        environment::read_environment(EnvironmentScope::User).map_err(error::command_error)?;
    let system =
        environment::read_environment(EnvironmentScope::System).map_err(error::command_error)?;
    let preferences = read_tool_root_preferences(&state.data_root)
        .map_err(|error| format!("读取工具目录配置失败：{error}"))?;
    let machine = diagnostics::machine_context(&state.data_root, &preferences, &user, &system);
    Ok(diagnostics::guidance_for(
        issue, &scan, &machine, &user, &system,
    ))
}

#[tauri::command]
async fn scan_environment(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<EnvironmentScan, String> {
    perform_environment_scan(&app, state.inner(), true).await
}

#[tauri::command]
async fn refresh_environment_scan(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<EnvironmentScan, String> {
    perform_environment_scan(&app, state.inner(), false).await
}

async fn perform_environment_scan(
    app: &AppHandle,
    state: &Arc<AppState>,
    force_disk_discovery: bool,
) -> Result<EnvironmentScan, String> {
    let registry = state.registry.clone();
    let data_root = state.data_root.clone();
    let scan_root = data_root.clone();
    let scan = tokio::task::spawn_blocking(move || {
        if force_disk_discovery {
            scanner::scan(&registry, &scan_root)
        } else {
            scanner::refresh(&registry, &scan_root)
        }
    })
    .await
    .map_err(|error| format!("扫描任务异常结束：{error}"))?
    .map_err(error::command_error)?;
    write_cached_environment_scan(&data_root, &scan)
        .map_err(|error| format!("扫描完成，但保存扫描快照失败：{error}"))?;
    refresh_tray_menu(app).map_err(|error| format!("扫描完成，但刷新托盘菜单失败：{error}"))?;
    Ok(scan)
}

#[tauri::command]
fn cached_environment_scan(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<EnvironmentScan>, String> {
    read_cached_environment_scan(&state.data_root)
        .map_err(|error| format!("读取上次扫描快照失败：{error}"))
}

#[tauri::command]
async fn export_diagnostic_report(
    path: PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        return Err("诊断报告必须保存到绝对路径的 .json 文件".to_string());
    }
    let scan = read_cached_environment_scan(&state.data_root)
        .map_err(|error| format!("读取上次扫描快照失败：{error}"))?
        .ok_or_else(|| "尚无扫描结果，请先手动点击“开始扫描”".to_string())?;
    let user =
        environment::read_environment(EnvironmentScope::User).map_err(error::command_error)?;
    let system =
        environment::read_environment(EnvironmentScope::System).map_err(error::command_error)?;
    let preferences = read_tool_root_preferences(&state.data_root)
        .map_err(|error| format!("读取工具目录配置失败：{error}"))?;
    let machine = diagnostics::machine_context(&state.data_root, &preferences, &user, &system);
    let guidance = scan
        .issues
        .iter()
        .chain(scan.tools.iter().flat_map(|tool| tool.issues.iter()))
        .map(|issue| diagnostics::guidance_for(issue, &scan, &machine, &user, &system))
        .collect();
    let report = DiagnosticReport {
        schema_version: 1,
        generated_at: chrono::Utc::now(),
        machine,
        scan,
        guidance,
    };
    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("序列化诊断报告失败：{error}"))?;
    write_bytes_atomic(&path, &bytes).map_err(|error| format!("写入诊断报告失败：{error}"))?;
    Ok(path)
}

#[tauri::command]
fn recent_operation_logs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<OperationLogEntry>, String> {
    let directory = state.data_root.join("logs");
    let mut entries = Vec::new();
    let files = fs::read_dir(directory).map_err(|error| format!("读取日志目录失败：{error}"))?;
    for file in files.flatten().filter(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
    }) {
        let Ok(file) = fs::File::open(file.path()) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(entry) = serde_json::from_str::<OperationLogEntry>(&line) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp));
    entries.truncate(100);
    Ok(entries)
}

#[tauri::command]
async fn fetch_versions(
    tool_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<VersionCatalog, String> {
    let plugin = state.registry.get(&tool_id).map_err(error::command_error)?;
    let fetched = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        plugin.fetch_available_versions(&state.client),
    )
    .await;
    let failure = match fetched {
        Ok(Ok(mut catalog)) if !catalog.versions.is_empty() => {
            versioning::sort_remote_versions_descending(&mut catalog.versions);
            let _ = write_version_catalog_cache(&state.data_root, &catalog);
            return Ok(catalog);
        }
        Ok(Ok(_)) => "官方源没有返回可安装的 Windows x64 版本".to_string(),
        Ok(Err(error)) => error.to_string(),
        Err(_) => "查询超过 45 秒，已停止等待".to_string(),
    };
    if let Ok(Some(catalog)) = read_version_catalog_cache(&state.data_root, &tool_id) {
        return Ok(catalog);
    }
    Err(format!(
        "官方版本源暂时不可用：{failure}。请检查网络或代理设置后重试"
    ))
}

#[tauri::command]
fn preview_switch(
    tool_id: String,
    installation_path: PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<OperationPlan, String> {
    state
        .plans
        .preview_switch(&state.registry, &tool_id, installation_path)
        .map_err(error::command_error)
}

#[tauri::command]
async fn preview_install(
    tool_id: String,
    version: String,
    root: PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<OperationPlan, String> {
    let plugin = state.registry.get(&tool_id).map_err(error::command_error)?;
    let catalog = plugin
        .fetch_available_versions(&state.client)
        .await
        .map_err(error::command_error)?;
    let remote = catalog
        .versions
        .iter()
        .find(|remote| remote.version == version)
        .ok_or_else(|| "所选版本不在刚刚查询的官方版本清单中".to_string())?;
    state
        .plans
        .preview_install(plugin.descriptor(), remote, root)
        .map_err(error::command_error)
}

#[tauri::command]
async fn preview_repair(
    tool_id: String,
    installation_path: PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<OperationPlan, String> {
    let plugin = state.registry.get(&tool_id).map_err(error::command_error)?;
    let manifest = state
        .installer
        .load_manifest(&installation_path)
        .map_err(error::command_error)?;
    if manifest.tool_id != tool_id {
        return Err("受管安装清单与工具 ID 不匹配".to_string());
    }
    let catalog = plugin
        .fetch_available_versions(&state.client)
        .await
        .map_err(error::command_error)?;
    let remote = catalog
        .versions
        .iter()
        .find(|remote| remote.version == manifest.version)
        .ok_or_else(|| "官方清单中已找不到该版本，不能安全地自动修复".to_string())?;
    state
        .plans
        .preview_repair(
            plugin.descriptor(),
            remote,
            manifest.managed_root,
            manifest.installation_path,
        )
        .map_err(error::command_error)
}

#[tauri::command]
fn preview_uninstall(
    tool_id: String,
    installation_path: PathBuf,
    state: State<'_, Arc<AppState>>,
) -> Result<OperationPlan, String> {
    state
        .plans
        .preview_uninstall(&state.registry, &tool_id, installation_path)
        .map_err(error::command_error)
}

#[tauri::command]
fn list_environment_backups(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<EnvironmentBackupSummary>, String> {
    state.plans.list_backups().map_err(error::command_error)
}

#[tauri::command]
fn preview_restore_environment(
    backup_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<OperationPlan, String> {
    state
        .plans
        .preview_restore(&backup_id)
        .map_err(error::command_error)
}

#[tauri::command]
fn preview_diagnostic_repair(
    issue_code: String,
    state: State<'_, Arc<AppState>>,
) -> Result<OperationPlan, String> {
    if issue_code.ends_with("_NO_DEFAULT") || issue_code == "JAVA_HOME_DEFAULT_MISMATCH" {
        let scan = read_cached_environment_scan(&state.data_root)
            .map_err(|error| format!("读取上次扫描快照失败：{error}"))?
            .ok_or_else(|| "尚无扫描结果，请先手动扫描".to_string())?;
        let tool_id = diagnostics::issue_tool_id(&issue_code)
            .ok_or_else(|| "无法确定该诊断项对应的工具".to_string())?;
        let tool = scan
            .tools
            .iter()
            .find(|tool| tool.id == tool_id)
            .ok_or_else(|| "扫描快照中找不到对应工具".to_string())?;
        let target = if issue_code == "JAVA_HOME_DEFAULT_MISMATCH" {
            tool.default_version.as_ref()
        } else if tool.installed_versions.len() == 1 {
            tool.installed_versions.first()
        } else {
            None
        }
        .ok_or_else(|| "存在多个候选版本，无法安全地自动选择；请在工具详情中选择".to_string())?;
        return state
            .plans
            .preview_switch(&state.registry, tool_id, target.path.clone())
            .map_err(error::command_error);
    }
    state
        .plans
        .preview_diagnostic_repair(&issue_code)
        .map_err(error::command_error)
}

#[tauri::command]
async fn apply_plan(
    plan_id: String,
    confirmation_token: String,
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<OperationResult, String> {
    let plan = state
        .plans
        .take_confirmed(&plan_id, &confirmation_token)
        .map_err(error::command_error)?;
    match &plan.action {
        PlannedAction::Switch { .. }
        | PlannedAction::RestoreEnvironment { .. }
        | PlannedAction::UpdateUserEnvironment { .. } => {
            state
                .plans
                .apply_environment_plan(&state.registry, &plan)
                .map_err(error::command_error)?;
            Ok(OperationResult {
                operation_id: plan.id,
                status: "committed".to_string(),
                message: match plan.action {
                    PlannedAction::Switch { .. } => "默认版本已切换；请打开新终端验证".to_string(),
                    PlannedAction::RestoreEnvironment { .. } => {
                        "用户环境备份已恢复；请打开新终端验证".to_string()
                    }
                    PlannedAction::UpdateUserEnvironment { reason, .. } => {
                        format!("{reason}；请打开新终端验证")
                    }
                    _ => unreachable!(),
                },
                installation_path: None,
            })
        }
        PlannedAction::Uninstall { .. } => {
            let rollback = state
                .plans
                .apply_uninstall_environment(&plan)
                .map_err(error::command_error)?;
            match state.installer.execute(&plan.id, &plan.action, &app).await {
                Ok(result) => Ok(result),
                Err(install_error) => {
                    if let Some(environment) = rollback
                        && let Err(rollback_error) =
                            state.plans.rollback_user_environment(&environment)
                    {
                        return Err(format!(
                            "卸载失败：{install_error}；用户环境回滚也失败：{rollback_error}"
                        ));
                    }
                    Err(error::command_error(install_error))
                }
            }
        }
        _ => state
            .installer
            .execute(&plan.id, &plan.action, &app)
            .await
            .map_err(error::command_error),
    }
}

pub fn run() {
    let data_root = resolve_data_root();
    if let Err(error) = ensure_data_layout(&data_root) {
        panic!(
            "无法初始化 EnvNexus AI 数据目录 {}：{error}",
            data_root.display()
        );
    }
    application_update::cleanup_stale_updates(&data_root);
    let client = reqwest::Client::builder()
        .user_agent(format!("EnvNexus-AI/{}", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("无法创建 HTTP 客户端");
    let download_client = reqwest::Client::builder()
        .user_agent(format!("EnvNexus-AI/{}", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .connect_timeout(std::time::Duration::from_secs(20))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("无法创建下载 HTTP 客户端");
    let ai_client = reqwest::Client::builder()
        .user_agent(format!("EnvNexus-AI/{}", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("无法创建 AI HTTP 客户端");
    let preferences = app_preferences::read(&data_root).unwrap_or_default();
    let state = Arc::new(AppState {
        registry: PluginRegistry::builtin(),
        plans: PlanService::new(data_root.clone()),
        installer: Installer::new(download_client.clone(), data_root.clone()),
        update_client: download_client,
        client,
        ai_client,
        data_root,
        app_preferences: RwLock::new(preferences),
        tray_actions: RwLock::new(HashMap::new()),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            restore_existing_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .setup(|app| {
            setup_tray(app)?;
            Ok(())
        })
        .on_window_event(handle_window_event)
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            app_preferences,
            save_app_preferences,
            hide_to_tray,
            restore_main_window,
            tray_ready,
            tray_menu_status,
            tool_definitions,
            configure_data_root,
            tool_root_preferences,
            set_tool_root,
            set_android_root,
            terminal_commands_status,
            prepare_terminal_commands,
            save_terminal_command_directory,
            preview_enable_terminal_commands,
            preview_disable_terminal_commands,
            ai_settings,
            save_ai_provider,
            clear_ai_api_key,
            select_ai_model,
            activate_ai_provider,
            fetch_ai_models,
            analyze_diagnostic_with_ai,
            diagnostic_guidance,
            scan_environment,
            refresh_environment_scan,
            cached_environment_scan,
            export_diagnostic_report,
            recent_operation_logs,
            fetch_versions,
            preview_switch,
            preview_install,
            preview_repair,
            preview_uninstall,
            list_environment_backups,
            preview_restore_environment,
            preview_diagnostic_repair,
            apply_plan,
            prepare_application_update,
            launch_application_update,
            confirm_application_update_started
        ])
        .run(tauri::generate_context!())
        .expect("运行 EnvNexus AI 时发生错误");
}

fn setup_tray<R: Runtime>(app: &mut tauri::App<R>) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;
    let language = current_app_language(app.handle());

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(tray_text(language, "tooltip"))
        .on_menu_event(|app, event| handle_tray_menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) {
                restore_existing_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn build_tray_menu<R: Runtime, M: Manager<R>>(manager: &M) -> tauri::Result<Menu<R>> {
    let app = manager.app_handle();
    let state = app.state::<Arc<AppState>>();
    let language = state
        .app_preferences
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .language;
    let cached_scan = read_cached_environment_scan(&state.data_root).unwrap_or_default();
    let mut actions = HashMap::new();
    let menu = Menu::new(manager)?;

    let open = IconMenuItem::with_id(
        manager,
        "tray_open",
        tray_text(language, "open"),
        true,
        Some(menu_asset_icon("open")),
        None::<&str>,
    )?;
    menu.append(&open)?;

    let tools = Submenu::with_id_and_icon(
        manager,
        "tray_tools",
        tray_text(language, "tools"),
        true,
        Some(menu_asset_icon("tools")),
    )?;
    let tools_overview = IconMenuItem::with_id(
        manager,
        "tray_tools_overview",
        tray_text(language, "tools_overview"),
        true,
        Some(menu_asset_icon("dashboard")),
        None::<&str>,
    )?;
    tools.append(&tools_overview)?;
    tools.append(&PredefinedMenuItem::separator(manager)?)?;

    for (tool_index, plugin) in state.registry.all().iter().enumerate() {
        let descriptor = plugin.descriptor();
        let tool_icon = tool_menu_asset_icon(descriptor.id);
        let tool_menu = Submenu::with_id_and_icon(
            manager,
            format!("tray_tool_menu_{}", descriptor.id),
            descriptor.display_name,
            true,
            Some(tool_icon.clone()),
        )?;
        let open_id = format!("tray_dynamic_open_{tool_index}");
        actions.insert(
            open_id.clone(),
            TrayCommand::OpenTool(descriptor.id.to_string()),
        );
        let open_tool = IconMenuItem::with_id(
            manager,
            open_id,
            tray_open_tool_text(language, descriptor.display_name),
            true,
            Some(tool_icon.clone()),
            None::<&str>,
        )?;
        tool_menu.append(&open_tool)?;
        tool_menu.append(&PredefinedMenuItem::separator(manager)?)?;

        let inventory = cached_scan
            .as_ref()
            .and_then(|scan| scan.tools.iter().find(|tool| tool.id == descriptor.id));
        match inventory {
            Some(inventory) if !inventory.installed_versions.is_empty() => {
                for (version_index, version) in inventory.installed_versions.iter().enumerate() {
                    let item_id = format!("tray_dynamic_switch_{tool_index}_{version_index}");
                    let label = if version.is_default {
                        format!(
                            "{}  ✓ {}",
                            version.version,
                            tray_text(language, "default_version")
                        )
                    } else {
                        version.version.clone()
                    };
                    if !version.is_default {
                        actions.insert(
                            item_id.clone(),
                            TrayCommand::PreviewSwitch {
                                tool_id: descriptor.id.to_string(),
                                installation_path: version.path.clone(),
                            },
                        );
                    }
                    let version_item = IconMenuItem::with_id(
                        manager,
                        item_id,
                        label,
                        !version.is_default,
                        Some(menu_asset_icon(if version.is_default {
                            "default"
                        } else {
                            "version"
                        })),
                        None::<&str>,
                    )?;
                    tool_menu.append(&version_item)?;
                }
            }
            Some(_) => {
                let empty = IconMenuItem::with_id(
                    manager,
                    format!("tray_empty_{tool_index}"),
                    tray_text(language, "no_installed_versions"),
                    false,
                    Some(menu_asset_icon("info")),
                    None::<&str>,
                )?;
                tool_menu.append(&empty)?;
            }
            None => {
                let unscanned = IconMenuItem::with_id(
                    manager,
                    format!("tray_unscanned_{tool_index}"),
                    tray_text(language, "not_scanned"),
                    false,
                    Some(menu_asset_icon("info")),
                    None::<&str>,
                )?;
                tool_menu.append(&unscanned)?;
            }
        }
        tools.append(&tool_menu)?;
    }
    menu.append(&tools)?;

    let diagnostics = Submenu::with_id_and_icon(
        manager,
        "tray_diagnostics",
        tray_text(language, "diagnostics_management"),
        true,
        Some(menu_asset_icon("diagnostics")),
    )?;
    let diagnostics_overview = IconMenuItem::with_id(
        manager,
        "tray_diagnostics_overview",
        tray_text(language, "diagnostics_overview"),
        true,
        Some(menu_asset_icon("diagnostics")),
        None::<&str>,
    )?;
    diagnostics.append(&diagnostics_overview)?;
    diagnostics.append(&PredefinedMenuItem::separator(manager)?)?;
    match cached_scan.as_ref() {
        Some(scan) => {
            let issues = scan
                .issues
                .iter()
                .chain(scan.tools.iter().flat_map(|tool| tool.issues.iter()))
                .collect::<Vec<_>>();
            if issues.is_empty() {
                diagnostics.append(&IconMenuItem::with_id(
                    manager,
                    "tray_diagnostics_empty",
                    tray_text(language, "diagnostics_empty"),
                    false,
                    Some(menu_asset_icon("default")),
                    None::<&str>,
                )?)?;
            } else {
                for (issue_index, issue) in issues.iter().enumerate() {
                    let issue_icon_name = match issue.level {
                        model::IssueLevel::Error => "error",
                        model::IssueLevel::Warning => "warning",
                        model::IssueLevel::Info => "info",
                    };
                    let issue_menu = Submenu::with_id_and_icon(
                        manager,
                        format!("tray_diagnostic_issue_{issue_index}"),
                        &issue.title,
                        true,
                        Some(menu_asset_icon(issue_icon_name)),
                    )?;
                    let view_id = format!("tray_dynamic_diagnostic_view_{issue_index}");
                    actions.insert(
                        view_id.clone(),
                        TrayCommand::OpenDiagnostic(issue.code.clone()),
                    );
                    issue_menu.append(&IconMenuItem::with_id(
                        manager,
                        view_id,
                        tray_text(language, "diagnostic_view"),
                        true,
                        Some(menu_asset_icon("view")),
                        None::<&str>,
                    )?)?;
                    if issue.repairable {
                        let repair_id = format!("tray_dynamic_diagnostic_repair_{issue_index}");
                        actions.insert(
                            repair_id.clone(),
                            TrayCommand::PreviewDiagnosticRepair(issue.code.clone()),
                        );
                        issue_menu.append(&IconMenuItem::with_id(
                            manager,
                            repair_id,
                            tray_text(language, "diagnostic_repair"),
                            true,
                            Some(menu_asset_icon("repair")),
                            None::<&str>,
                        )?)?;
                    } else {
                        issue_menu.append(&IconMenuItem::with_id(
                            manager,
                            format!("tray_diagnostic_manual_{issue_index}"),
                            tray_text(language, "diagnostic_manual"),
                            false,
                            Some(menu_asset_icon("info")),
                            None::<&str>,
                        )?)?;
                    }
                    diagnostics.append(&issue_menu)?;
                }
            }
        }
        None => {
            diagnostics.append(&IconMenuItem::with_id(
                manager,
                "tray_diagnostics_unscanned",
                tray_text(language, "diagnostics_unscanned"),
                false,
                Some(menu_asset_icon("info")),
                None::<&str>,
            )?)?;
        }
    }
    let ai_services = Submenu::with_id_and_icon(
        manager,
        "tray_ai_services",
        tray_text(language, "ai_services"),
        true,
        Some(menu_asset_icon("ai")),
    )?;
    ai_services.append(&IconMenuItem::with_id(
        manager,
        "tray_ai_settings",
        tray_text(language, "ai_settings"),
        true,
        Some(menu_asset_icon("settings")),
        None::<&str>,
    )?)?;
    ai_services.append(&PredefinedMenuItem::separator(manager)?)?;
    let ai_settings = ai::read_settings(&state.data_root).ok();
    let valid_ai_providers = ai_settings
        .as_ref()
        .map(|settings| {
            settings
                .providers
                .iter()
                .filter(|provider| provider.api_key_configured && provider.selected_model.is_some())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if valid_ai_providers.is_empty() {
        ai_services.append(&IconMenuItem::with_id(
            manager,
            "tray_ai_empty",
            tray_text(language, "ai_empty"),
            false,
            Some(menu_asset_icon("info")),
            None::<&str>,
        )?)?;
    } else {
        for (provider_index, provider) in valid_ai_providers.iter().enumerate() {
            let item_id = format!("tray_dynamic_ai_provider_{provider_index}");
            let is_active = ai_settings
                .as_ref()
                .and_then(|settings| settings.active_provider_id.as_deref())
                == Some(provider.id.as_str());
            let model = provider.selected_model.as_deref().unwrap_or_default();
            let label = if is_active {
                format!(
                    "{} · {}  ✓ {}",
                    provider.display_name,
                    model,
                    tray_text(language, "ai_current")
                )
            } else {
                format!("{} · {}", provider.display_name, model)
            };
            actions.insert(
                item_id.clone(),
                TrayCommand::SelectAiProvider(provider.id.clone()),
            );
            ai_services.append(&IconMenuItem::with_id(
                manager,
                item_id,
                label,
                !is_active,
                Some(ai_menu_asset_icon(&provider.id)),
                None::<&str>,
            )?)?;
        }
    }
    let settings = IconMenuItem::with_id(
        manager,
        "tray_settings",
        tray_text(language, "settings"),
        true,
        Some(menu_asset_icon("settings")),
        None::<&str>,
    )?;
    let scan = IconMenuItem::with_id(
        manager,
        "tray_scan",
        tray_text(language, "scan"),
        true,
        Some(menu_asset_icon("scan")),
        None::<&str>,
    )?;
    let exit = IconMenuItem::with_id(
        manager,
        "tray_exit",
        tray_text(language, "exit"),
        true,
        Some(menu_asset_icon("exit")),
        None::<&str>,
    )?;
    menu.append(&diagnostics)?;
    menu.append(&ai_services)?;
    menu.append(&settings)?;
    menu.append(&scan)?;
    menu.append(&PredefinedMenuItem::separator(manager)?)?;
    menu.append(&exit)?;

    *state
        .tray_actions
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = actions;
    Ok(menu)
}

fn refresh_tray_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(build_tray_menu(app)?))?;
        tray.set_tooltip(Some(tray_text(current_app_language(app), "tooltip")))?;
    }
    Ok(())
}

fn handle_tray_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        "tray_open" => restore_existing_main_window(app),
        "tray_tools_overview" => show_main_window(
            app,
            TrayFrontendAction::Navigate {
                view: "tools".to_string(),
            },
        ),
        "tray_diagnostics_overview" => show_main_window(
            app,
            TrayFrontendAction::Navigate {
                view: "diagnostics".to_string(),
            },
        ),
        "tray_settings" | "tray_ai_settings" => show_main_window(
            app,
            TrayFrontendAction::Navigate {
                view: "settings".to_string(),
            },
        ),
        "tray_scan" => show_main_window(app, TrayFrontendAction::Scan),
        "tray_exit" => app.exit(0),
        _ => {
            let action = {
                let state = app.state::<Arc<AppState>>();
                state
                    .tray_actions
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(id)
                    .cloned()
            };
            match action {
                Some(TrayCommand::OpenTool(tool_id)) => {
                    show_main_window(app, TrayFrontendAction::OpenTool { tool_id });
                }
                Some(TrayCommand::PreviewSwitch {
                    tool_id,
                    installation_path,
                }) => show_main_window(
                    app,
                    TrayFrontendAction::PreviewSwitch {
                        tool_id,
                        installation_path,
                    },
                ),
                Some(TrayCommand::OpenDiagnostic(issue_code)) => {
                    show_main_window(app, TrayFrontendAction::OpenDiagnostic { issue_code })
                }
                Some(TrayCommand::PreviewDiagnosticRepair(issue_code)) => show_main_window(
                    app,
                    TrayFrontendAction::PreviewDiagnosticRepair { issue_code },
                ),
                Some(TrayCommand::SelectAiProvider(provider_id)) => {
                    let state = app.state::<Arc<AppState>>();
                    if ai::activate_provider(&state.data_root, &provider_id).is_ok() {
                        let _ = refresh_tray_menu(app);
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.emit(
                                "tray-action",
                                TrayFrontendAction::SelectAiProvider { provider_id },
                            );
                        }
                    } else {
                        show_main_window(
                            app,
                            TrayFrontendAction::Navigate {
                                view: "settings".to_string(),
                            },
                        );
                    }
                }
                None => {}
            }
        }
    }
}

fn restore_existing_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        set_webview_low_memory_target(app, false);
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>, action: TrayFrontendAction) {
    if let Some(window) = app.get_webview_window("main") {
        set_webview_low_memory_target(app, false);
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        let _ = app.emit("tray-action", action);
    }
}

fn current_app_language<R: Runtime>(app: &AppHandle<R>) -> AppLanguage {
    app.state::<Arc<AppState>>()
        .app_preferences
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .language
}

fn tray_text(language: AppLanguage, key: &str) -> &'static str {
    match language {
        AppLanguage::SimplifiedChinese => match key {
            "tooltip" => "EnvNexus AI 开发环境管理",
            "open" => "打开 EnvNexus AI",
            "tools" => "工具链",
            "tools_overview" => "打开工具链总览",
            "diagnostics" => "诊断与日志",
            "diagnostics_management" => "诊断管理",
            "diagnostics_overview" => "打开诊断与日志",
            "diagnostics_empty" => "上次扫描未发现问题",
            "diagnostics_unscanned" => "尚未扫描，无诊断快照",
            "diagnostic_view" => "查看诊断详情",
            "diagnostic_repair" => "生成诊断修复计划",
            "diagnostic_manual" => "查看建议后人工处理",
            "ai_services" => "AI 服务",
            "ai_settings" => "打开 AI 配置",
            "ai_empty" => "没有可用的 AI 配置",
            "ai_current" => "当前",
            "settings" => "设置",
            "scan" => "手动重新扫描",
            "exit" => "退出 EnvNexus AI",
            "default_version" => "默认",
            "no_installed_versions" => "未发现已安装版本",
            "not_scanned" => "尚未扫描（使用上次结果）",
            _ => "",
        },
        AppLanguage::TraditionalChinese => match key {
            "tooltip" => "EnvNexus AI 開發環境管理",
            "open" => "開啟 EnvNexus AI",
            "tools" => "工具鏈",
            "tools_overview" => "開啟工具鏈總覽",
            "diagnostics" => "診斷與日誌",
            "diagnostics_management" => "診斷管理",
            "diagnostics_overview" => "開啟診斷與日誌",
            "diagnostics_empty" => "上次掃描未發現問題",
            "diagnostics_unscanned" => "尚未掃描，沒有診斷快照",
            "diagnostic_view" => "檢視診斷詳情",
            "diagnostic_repair" => "產生診斷修復計畫",
            "diagnostic_manual" => "檢視建議後手動處理",
            "ai_services" => "AI 服務",
            "ai_settings" => "開啟 AI 設定",
            "ai_empty" => "沒有可用的 AI 設定",
            "ai_current" => "目前",
            "settings" => "設定",
            "scan" => "手動重新掃描",
            "exit" => "結束 EnvNexus AI",
            "default_version" => "預設",
            "no_installed_versions" => "未發現已安裝版本",
            "not_scanned" => "尚未掃描（使用上次結果）",
            _ => "",
        },
        AppLanguage::English => match key {
            "tooltip" => "EnvNexus AI Development Environment Manager",
            "open" => "Open EnvNexus AI",
            "tools" => "Toolchains",
            "tools_overview" => "Open toolchain overview",
            "diagnostics" => "Diagnostics and logs",
            "diagnostics_management" => "Diagnostic management",
            "diagnostics_overview" => "Open diagnostics and logs",
            "diagnostics_empty" => "No issues in the last scan",
            "diagnostics_unscanned" => "Not scanned; no diagnostic snapshot",
            "diagnostic_view" => "View diagnostic details",
            "diagnostic_repair" => "Create diagnostic repair plan",
            "diagnostic_manual" => "Review guidance and resolve manually",
            "ai_services" => "AI services",
            "ai_settings" => "Open AI settings",
            "ai_empty" => "No usable AI provider",
            "ai_current" => "Current",
            "settings" => "Settings",
            "scan" => "Scan now",
            "exit" => "Exit EnvNexus AI",
            "default_version" => "Default",
            "no_installed_versions" => "No installed versions found",
            "not_scanned" => "Not scanned (using cached results)",
            _ => "",
        },
        AppLanguage::Japanese => match key {
            "tooltip" => "EnvNexus AI 開発環境マネージャー",
            "open" => "EnvNexus AI を開く",
            "tools" => "ツールチェーン",
            "tools_overview" => "ツールチェーン一覧を開く",
            "diagnostics" => "診断とログ",
            "diagnostics_management" => "診断管理",
            "diagnostics_overview" => "診断とログを開く",
            "diagnostics_empty" => "前回のスキャンで問題はありません",
            "diagnostics_unscanned" => "未スキャン：診断スナップショットなし",
            "diagnostic_view" => "診断の詳細を表示",
            "diagnostic_repair" => "診断修復プランを作成",
            "diagnostic_manual" => "提案を確認して手動対応",
            "ai_services" => "AI サービス",
            "ai_settings" => "AI 設定を開く",
            "ai_empty" => "利用可能な AI 設定なし",
            "ai_current" => "使用中",
            "settings" => "設定",
            "scan" => "今すぐスキャン",
            "exit" => "EnvNexus AI を終了",
            "default_version" => "既定",
            "no_installed_versions" => "インストール済みバージョンなし",
            "not_scanned" => "未スキャン（キャッシュを使用）",
            _ => "",
        },
        AppLanguage::Korean => match key {
            "tooltip" => "EnvNexus AI 개발 환경 관리자",
            "open" => "EnvNexus AI 열기",
            "tools" => "도구 체인",
            "tools_overview" => "도구 체인 개요 열기",
            "diagnostics" => "진단 및 로그",
            "diagnostics_management" => "진단 관리",
            "diagnostics_overview" => "진단 및 로그 열기",
            "diagnostics_empty" => "마지막 스캔에서 문제 없음",
            "diagnostics_unscanned" => "스캔 안 됨: 진단 스냅샷 없음",
            "diagnostic_view" => "진단 세부 정보 보기",
            "diagnostic_repair" => "진단 복구 계획 생성",
            "diagnostic_manual" => "지침 확인 후 수동 처리",
            "ai_services" => "AI 서비스",
            "ai_settings" => "AI 설정 열기",
            "ai_empty" => "사용 가능한 AI 설정 없음",
            "ai_current" => "현재",
            "settings" => "설정",
            "scan" => "지금 스캔",
            "exit" => "EnvNexus AI 종료",
            "default_version" => "기본",
            "no_installed_versions" => "설치된 버전 없음",
            "not_scanned" => "스캔 안 됨(캐시 사용)",
            _ => "",
        },
    }
}

fn tray_open_tool_text(language: AppLanguage, display_name: &str) -> String {
    match language {
        AppLanguage::SimplifiedChinese => format!("打开 {display_name} 管理界面"),
        AppLanguage::TraditionalChinese => format!("開啟 {display_name} 管理介面"),
        AppLanguage::English => format!("Open {display_name} manager"),
        AppLanguage::Japanese => format!("{display_name} 管理画面を開く"),
        AppLanguage::Korean => format!("{display_name} 관리 화면 열기"),
    }
}

fn menu_asset_icon(name: &str) -> Image<'static> {
    let bytes: &'static [u8] = match name {
        "open" => include_bytes!("../icons/menu/open.png"),
        "dashboard" => include_bytes!("../icons/menu/dashboard.png"),
        "tools" => include_bytes!("../icons/menu/tools.png"),
        "diagnostics" => include_bytes!("../icons/menu/diagnostics.png"),
        "settings" => include_bytes!("../icons/menu/settings.png"),
        "scan" => include_bytes!("../icons/menu/scan.png"),
        "exit" => include_bytes!("../icons/menu/exit.png"),
        "version" => include_bytes!("../icons/menu/version.png"),
        "default" => include_bytes!("../icons/menu/default.png"),
        "view" => include_bytes!("../icons/menu/view.png"),
        "repair" => include_bytes!("../icons/menu/repair.png"),
        "warning" => include_bytes!("../icons/menu/warning.png"),
        "error" => include_bytes!("../icons/menu/error.png"),
        "ai" => include_bytes!("../icons/menu/ai.png"),
        _ => include_bytes!("../icons/menu/info.png"),
    };
    Image::from_bytes(bytes).expect("内置托盘功能图标必须是有效 PNG")
}

fn tool_menu_asset_icon(tool_id: &str) -> Image<'static> {
    let bytes: &'static [u8] = match tool_id {
        "python" => include_bytes!("../icons/menu/python.png"),
        "java" => include_bytes!("../icons/menu/java.png"),
        "go" => include_bytes!("../icons/menu/go.png"),
        "rust" => include_bytes!("../icons/menu/rust.png"),
        "node" => include_bytes!("../icons/menu/node.png"),
        "git" => include_bytes!("../icons/menu/git.png"),
        "maven" => include_bytes!("../icons/menu/maven.png"),
        "dotnet" => include_bytes!("../icons/menu/dotnet.png"),
        "ruby" => include_bytes!("../icons/menu/ruby.png"),
        "php" => include_bytes!("../icons/menu/php.png"),
        "android-sdk" => include_bytes!("../icons/menu/android-sdk.png"),
        "android-ndk" => include_bytes!("../icons/menu/android-ndk.png"),
        "gradle" => include_bytes!("../icons/menu/gradle.png"),
        "cmake" => include_bytes!("../icons/menu/cmake.png"),
        "adb" => include_bytes!("../icons/menu/adb.png"),
        _ => include_bytes!("../icons/menu/tools.png"),
    };
    Image::from_bytes(bytes).expect("内置托盘工具图标必须是有效 PNG")
}

fn ai_menu_asset_icon(provider_id: &str) -> Image<'static> {
    let bytes: &'static [u8] = match provider_id {
        "openai" => include_bytes!("../icons/menu/ai-openai.png"),
        "anthropic" => include_bytes!("../icons/menu/ai-anthropic.png"),
        "kimi" => include_bytes!("../icons/menu/ai-kimi.png"),
        "deepseek" => include_bytes!("../icons/menu/ai-deepseek.png"),
        "glm" => include_bytes!("../icons/menu/ai-glm.png"),
        "grok" => include_bytes!("../icons/menu/ai-grok.png"),
        "qwen" => include_bytes!("../icons/menu/ai-qwen.png"),
        "gemini" => include_bytes!("../icons/menu/ai-gemini.png"),
        _ => include_bytes!("../icons/menu/ai-custom.png"),
    };
    Image::from_bytes(bytes).expect("内置 AI 厂商托盘图标必须是有效 PNG")
}

fn handle_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if let WindowEvent::Focused(focused) = event {
        set_webview_low_memory_target(window.app_handle(), !focused);
    }
    let preferences = {
        let state = window.app_handle().state::<Arc<AppState>>();
        state
            .app_preferences
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    };
    if let WindowEvent::CloseRequested { api, .. } = event
        && preferences.close_behavior == CloseBehavior::MinimizeToTray
    {
        api.prevent_close();
        let _ = window.hide();
        set_webview_low_memory_target(window.app_handle(), true);
    }
}

#[cfg(windows)]
fn set_webview_low_memory_target<R: Runtime>(app: &AppHandle<R>, low_memory: bool) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.with_webview(move |webview| {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL, ICoreWebView2_19,
        };
        use windows_core::Interface;

        let Ok(core_webview) = (unsafe { webview.controller().CoreWebView2() }) else {
            return;
        };
        let Ok(core_webview) = core_webview.cast::<ICoreWebView2_19>() else {
            return;
        };
        let target = if low_memory {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
        } else {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
        };
        let _ = unsafe { core_webview.SetMemoryUsageTargetLevel(target) };
    });
}

#[cfg(not(windows))]
fn set_webview_low_memory_target<R: Runtime>(_app: &AppHandle<R>, _low_memory: bool) {}

fn resolve_data_root() -> PathBuf {
    if let Some(root) =
        std::env::var_os("ENVNEXUS_AI_DATA_ROOT").or_else(|| std::env::var_os("ENVPILOT_DATA_ROOT"))
    {
        return paths::simplify(PathBuf::from(root));
    }
    for pointer_path in data_root_pointer_paths() {
        if let Ok(bytes) = fs::read(pointer_path)
            && let Ok(pointer) = serde_json::from_slice::<DataRootPointer>(&bytes)
            && pointer.schema_version == 1
            && pointer.data_root.is_absolute()
        {
            return paths::simplify(pointer.data_root);
        }
    }
    if cfg!(debug_assertions) {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri 应位于项目目录下");
        let preferred = project_root.join(".envnexus-ai-data");
        let legacy = project_root.join(".envpilot-data");
        return if legacy.exists() && !preferred.exists() {
            legacy
        } else {
            preferred
        };
    }
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let preferred = executable_directory.join("EnvNexusAIData");
    let legacy = executable_directory.join("EnvPilotData");
    if legacy.exists() && !preferred.exists() {
        legacy
    } else {
        preferred
    }
}

fn data_root_pointer_path() -> Option<PathBuf> {
    data_root_pointer_paths().into_iter().next()
}

fn data_root_pointer_paths() -> Vec<PathBuf> {
    if cfg!(debug_assertions) {
        return Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|path| {
                vec![
                    path.join(".envnexus-ai-data-root.json"),
                    path.join(".envpilot-data-root.json"),
                ]
            })
            .unwrap_or_default();
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent().map(|parent| {
                vec![
                    parent.join("envnexus-ai-data-root.json"),
                    parent.join("envpilot-data-root.json"),
                ]
            })
        })
        .unwrap_or_default()
}

fn write_pointer_atomic(path: &Path, pointer: &DataRootPointer) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(pointer).map_err(std::io::Error::other)?;
    write_bytes_atomic(path, &bytes)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("target path has no parent"))?;
    if !parent.is_dir() {
        return Err(std::io::Error::other("target parent does not exist"));
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)?;
    if !path.exists() {
        return fs::rename(temporary, path);
    }
    let previous = path.with_extension("json.previous");
    if previous.exists() {
        fs::remove_file(&previous)?;
    }
    fs::rename(path, &previous)?;
    match fs::rename(&temporary, path) {
        Ok(()) => {
            let _ = fs::remove_file(previous);
            Ok(())
        }
        Err(error) => {
            let _ = fs::rename(previous, path);
            Err(error)
        }
    }
}

fn normalize_install_root(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err("安装目录必须是绝对路径，且不能是磁盘根目录".to_string());
    }
    fs::create_dir_all(&path).map_err(|error| format!("无法创建安装目录：{error}"))?;
    let canonical = paths::canonicalize_simplified(&path)
        .map_err(|error| format!("无法解析安装目录：{error}"))?;
    if canonical.parent().is_none() {
        return Err("不能把磁盘根目录作为工具安装目录".to_string());
    }
    Ok(canonical)
}

fn read_tool_root_preferences(root: &Path) -> std::io::Result<ToolRootPreferences> {
    let path = root.join("config").join("tool-roots.json");
    if !path.is_file() {
        return Ok(ToolRootPreferences::default());
    }
    let bytes = fs::read(path)?;
    let mut preferences =
        serde_json::from_slice::<ToolRootPreferences>(&bytes).map_err(std::io::Error::other)?;
    if preferences.schema_version != 1 {
        return Err(std::io::Error::other(format!(
            "unsupported tool root schema {}",
            preferences.schema_version
        )));
    }
    for configured_root in preferences.roots.values_mut() {
        *configured_root = paths::simplify(std::mem::take(configured_root));
    }
    if let Some(android_root) = preferences.android_root.take() {
        preferences.android_root = Some(paths::simplify(android_root));
    }
    Ok(preferences)
}

fn write_tool_root_preferences(
    root: &Path,
    preferences: &ToolRootPreferences,
) -> std::io::Result<()> {
    let path = root.join("config").join("tool-roots.json");
    let bytes = serde_json::to_vec_pretty(preferences).map_err(std::io::Error::other)?;
    write_bytes_atomic(&path, &bytes)
}

fn read_cached_environment_scan(root: &Path) -> std::io::Result<Option<EnvironmentScan>> {
    let path = root.join("cache").join("last-environment-scan.json");
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    let mut scan =
        serde_json::from_slice::<EnvironmentScan>(&bytes).map_err(std::io::Error::other)?;
    for tool in &mut scan.tools {
        if let Some(default_version) = &mut tool.default_version {
            simplify_installed_version_paths(default_version);
        }
        for installed_version in &mut tool.installed_versions {
            simplify_installed_version_paths(installed_version);
        }
        versioning::sort_installed_versions_descending(&mut tool.installed_versions);
    }
    for manager in &mut scan.version_managers {
        manager.executable = manager.executable.take().map(paths::simplify);
        manager.root = manager.root.take().map(paths::simplify);
    }
    Ok(Some(scan))
}

fn simplify_installed_version_paths(version: &mut model::InstalledVersion) {
    version.path = paths::simplify(std::mem::take(&mut version.path));
    version.executable = version.executable.take().map(paths::simplify);
}

fn write_cached_environment_scan(root: &Path, scan: &EnvironmentScan) -> std::io::Result<()> {
    let path = root.join("cache").join("last-environment-scan.json");
    let bytes = serde_json::to_vec_pretty(scan).map_err(std::io::Error::other)?;
    write_bytes_atomic(&path, &bytes)
}

fn version_catalog_cache_path(root: &Path, tool_id: &str) -> PathBuf {
    root.join("cache")
        .join("version-sources")
        .join(format!("{tool_id}.json"))
}

fn write_version_catalog_cache(root: &Path, catalog: &VersionCatalog) -> std::io::Result<()> {
    let path = version_catalog_cache_path(root, &catalog.tool_id);
    let bytes = serde_json::to_vec_pretty(catalog).map_err(std::io::Error::other)?;
    write_bytes_atomic(&path, &bytes)
}

fn read_version_catalog_cache(
    root: &Path,
    tool_id: &str,
) -> std::io::Result<Option<VersionCatalog>> {
    let path = version_catalog_cache_path(root, tool_id);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    let mut catalog =
        serde_json::from_slice::<VersionCatalog>(&bytes).map_err(std::io::Error::other)?;
    if catalog.tool_id != tool_id || catalog.versions.is_empty() {
        return Ok(None);
    }
    versioning::sort_remote_versions_descending(&mut catalog.versions);
    catalog.cached = true;
    Ok(Some(catalog))
}

fn ensure_data_layout(root: &Path) -> std::io::Result<()> {
    for directory in [
        "config",
        "config/installations",
        "cache/version-sources",
        "downloads",
        "logs",
        "backups/environment",
        "transactions",
        "tools",
        "commands",
        "updates",
    ] {
        fs::create_dir_all(root.join(directory))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_root_preferences_round_trip_in_data_root() {
        let temporary = tempfile::tempdir().unwrap();
        ensure_data_layout(temporary.path()).unwrap();
        let mut preferences = ToolRootPreferences::default();
        preferences.roots.insert(
            "python".to_string(),
            temporary.path().join("python-toolchains"),
        );
        preferences.android_root = Some(temporary.path().join("android"));
        for tool_id in ANDROID_WORKSPACE_TOOL_IDS {
            preferences
                .roots
                .insert(tool_id.to_string(), temporary.path().join("android"));
        }

        write_tool_root_preferences(temporary.path(), &preferences).unwrap();
        let restored = read_tool_root_preferences(temporary.path()).unwrap();

        assert_eq!(restored, preferences);
        assert!(
            restored.roots.keys().all(|tool_id| tool_id == "python"
                || ANDROID_WORKSPACE_TOOL_IDS.contains(&tool_id.as_str()))
        );
    }

    #[test]
    fn cached_version_catalog_is_returned_as_cached() {
        let temporary = tempfile::tempdir().unwrap();
        ensure_data_layout(temporary.path()).unwrap();
        let catalog = VersionCatalog {
            tool_id: "python".to_string(),
            source_name: "Python.org".to_string(),
            source_url: "https://www.python.org/".to_string(),
            fetched_at: chrono::Utc::now(),
            cached: false,
            versions: vec![crate::model::RemoteVersion {
                version: "3.13.14".to_string(),
                channel: "stable".to_string(),
                published_at: None,
                architecture: "x86_64".to_string(),
                download_url: Some("https://www.python.org/ftp/python/example.zip".to_string()),
                checksum_algorithm: Some("sha256".to_string()),
                checksum: Some("abc123".to_string()),
                notes_url: None,
            }],
        };

        write_version_catalog_cache(temporary.path(), &catalog).unwrap();
        let restored = read_version_catalog_cache(temporary.path(), "python")
            .unwrap()
            .unwrap();

        assert!(restored.cached);
        assert_eq!(restored.versions[0].version, "3.13.14");
    }

    #[test]
    fn cached_scan_round_trip_does_not_trigger_a_new_scan() {
        let temporary = tempfile::tempdir().unwrap();
        ensure_data_layout(temporary.path()).unwrap();
        let now = chrono::Utc::now();
        let scan = EnvironmentScan {
            tools: Vec::new(),
            issues: Vec::new(),
            version_managers: Vec::new(),
            user_path_entries: 0,
            scan_started_at: now,
            scan_finished_at: now,
        };

        write_cached_environment_scan(temporary.path(), &scan).unwrap();
        let restored = read_cached_environment_scan(temporary.path())
            .unwrap()
            .unwrap();

        assert_eq!(restored.scan_finished_at, scan.scan_finished_at);
        assert!(restored.tools.is_empty());
    }
}
