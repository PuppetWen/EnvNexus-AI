import { invoke } from "@tauri-apps/api/core";
import type {
  AiDiagnosticAnalysis,
  AiModelInfo,
  AiProviderInput,
  AiSettings,
  AppPreferences,
  BootstrapState,
  DiagnosticGuidance,
  EnvironmentBackupSummary,
  EnvironmentScan,
  OperationPlan,
  OperationLogEntry,
  OperationResult,
  ToolRootPreferences,
  ToolDefinition,
  TerminalCommandStatus,
  VersionCatalog,
} from "./types";

export const backend = {
  bootstrap(): Promise<BootstrapState> {
    return invoke("bootstrap");
  },

  appPreferences(): Promise<AppPreferences> {
    return invoke("app_preferences");
  },

  saveAppPreferences(preferences: AppPreferences): Promise<AppPreferences> {
    return invoke("save_app_preferences", { preferences });
  },

  hideToTray(): Promise<void> {
    return invoke("hide_to_tray");
  },

  trayReady(): Promise<boolean> {
    return invoke("tray_ready");
  },

  toolDefinitions(): Promise<ToolDefinition[]> {
    return invoke("tool_definitions");
  },

  configureDataRoot(path: string): Promise<string> {
    return invoke("configure_data_root", { path });
  },

  toolRootPreferences(): Promise<ToolRootPreferences> {
    return invoke("tool_root_preferences");
  },

  setToolRoot(toolId: string, path: string): Promise<ToolRootPreferences> {
    return invoke("set_tool_root", { toolId, path });
  },

  setAndroidRoot(path: string): Promise<ToolRootPreferences> {
    return invoke("set_android_root", { path });
  },

  terminalCommandsStatus(): Promise<TerminalCommandStatus> {
    return invoke("terminal_commands_status");
  },

  prepareTerminalCommands(): Promise<TerminalCommandStatus> {
    return invoke("prepare_terminal_commands");
  },

  saveTerminalCommandDirectory(directory: string): Promise<TerminalCommandStatus> {
    return invoke("save_terminal_command_directory", { directory });
  },

  previewEnableTerminalCommands(): Promise<OperationPlan> {
    return invoke("preview_enable_terminal_commands");
  },

  previewDisableTerminalCommands(): Promise<OperationPlan> {
    return invoke("preview_disable_terminal_commands");
  },

  scanEnvironment(): Promise<EnvironmentScan> {
    return invoke("scan_environment");
  },

  cachedEnvironmentScan(): Promise<EnvironmentScan | null> {
    return invoke("cached_environment_scan");
  },

  aiSettings(): Promise<AiSettings> {
    return invoke("ai_settings");
  },

  saveAiProvider(input: AiProviderInput): Promise<AiSettings> {
    return invoke("save_ai_provider", { input });
  },

  clearAiApiKey(providerId: string): Promise<AiSettings> {
    return invoke("clear_ai_api_key", { providerId });
  },

  selectAiModel(providerId: string, model: string): Promise<AiSettings> {
    return invoke("select_ai_model", { providerId, model });
  },

  activateAiProvider(providerId: string): Promise<AiSettings> {
    return invoke("activate_ai_provider", { providerId });
  },

  fetchAiModels(providerId: string): Promise<AiModelInfo[]> {
    return invoke("fetch_ai_models", { providerId });
  },

  analyzeDiagnosticWithAi(issueCode: string): Promise<AiDiagnosticAnalysis> {
    return invoke("analyze_diagnostic_with_ai", { issueCode });
  },

  diagnosticGuidance(issueCode: string): Promise<DiagnosticGuidance> {
    return invoke("diagnostic_guidance", { issueCode });
  },

  exportDiagnosticReport(path: string): Promise<string> {
    return invoke("export_diagnostic_report", { path });
  },

  recentOperationLogs(): Promise<OperationLogEntry[]> {
    return invoke("recent_operation_logs");
  },

  fetchVersions(toolId: string): Promise<VersionCatalog> {
    return invoke("fetch_versions", { toolId });
  },

  previewSwitch(toolId: string, installationPath: string): Promise<OperationPlan> {
    return invoke("preview_switch", { toolId, installationPath });
  },

  previewInstall(toolId: string, version: string, root: string): Promise<OperationPlan> {
    return invoke("preview_install", { toolId, version, root });
  },

  previewRepair(toolId: string, installationPath: string): Promise<OperationPlan> {
    return invoke("preview_repair", { toolId, installationPath });
  },

  previewUninstall(toolId: string, installationPath: string): Promise<OperationPlan> {
    return invoke("preview_uninstall", { toolId, installationPath });
  },

  listEnvironmentBackups(): Promise<EnvironmentBackupSummary[]> {
    return invoke("list_environment_backups");
  },

  previewRestoreEnvironment(backupId: string): Promise<OperationPlan> {
    return invoke("preview_restore_environment", { backupId });
  },

  previewDiagnosticRepair(issueCode: string): Promise<OperationPlan> {
    return invoke("preview_diagnostic_repair", { issueCode });
  },

  applyPlan(planId: string, confirmationToken: string): Promise<OperationResult> {
    return invoke("apply_plan", { planId, confirmationToken });
  },
};
