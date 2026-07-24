use std::{collections::BTreeMap, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthLevel {
    Healthy,
    Warning,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticIssue {
    pub code: String,
    pub level: IssueLevel,
    pub title: String,
    pub detail: String,
    pub evidence: Option<String>,
    pub repairable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IssueLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledVersion {
    pub version: String,
    pub path: PathBuf,
    pub source: String,
    pub is_default: bool,
    pub managed: bool,
    pub health: HealthLevel,
    pub executable: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInventory {
    pub id: String,
    pub display_name: String,
    pub category: String,
    pub icon: String,
    pub capabilities: ToolCapabilities,
    pub default_version: Option<InstalledVersion>,
    pub installed_versions: Vec<InstalledVersion>,
    pub environment_status: HealthLevel,
    pub issues: Vec<DiagnosticIssue>,
    pub scanned_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub id: String,
    pub display_name: String,
    pub category: String,
    pub icon: String,
    pub capabilities: ToolCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapabilities {
    pub install: bool,
    pub switch_default: bool,
    pub repair: bool,
    pub uninstall: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentScan {
    pub tools: Vec<ToolInventory>,
    pub issues: Vec<DiagnosticIssue>,
    #[serde(default)]
    pub version_managers: Vec<VersionManagerInventory>,
    pub user_path_entries: usize,
    pub scan_started_at: DateTime<Utc>,
    pub scan_finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionManagerInventory {
    pub id: String,
    pub display_name: String,
    pub tool_ids: Vec<String>,
    pub executable: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub current_version: Option<String>,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteVersion {
    pub version: String,
    pub channel: String,
    pub published_at: Option<String>,
    pub architecture: String,
    pub download_url: Option<String>,
    pub checksum_algorithm: Option<String>,
    pub checksum: Option<String>,
    pub notes_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionCatalog {
    pub tool_id: String,
    pub source_name: String,
    pub source_url: String,
    pub fetched_at: DateTime<Utc>,
    pub cached: bool,
    pub versions: Vec<RemoteVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapState {
    pub app_version: String,
    pub data_root: PathBuf,
    pub config_ready: bool,
    pub platform: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CloseBehavior {
    MinimizeToTray,
    Exit,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppLanguage {
    #[default]
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "zh-TW")]
    TraditionalChinese,
    #[serde(rename = "en-US")]
    English,
    #[serde(rename = "ja-JP")]
    Japanese,
    #[serde(rename = "ko-KR")]
    Korean,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppPreferences {
    pub schema_version: u32,
    pub close_behavior: CloseBehavior,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default)]
    pub launch_at_login: bool,
    #[serde(default)]
    pub language: AppLanguage,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            close_behavior: CloseBehavior::Exit,
            start_minimized: false,
            launch_at_login: false,
            language: AppLanguage::SimplifiedChinese,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCommandStatus {
    pub directory: PathBuf,
    pub enabled_in_user_path: bool,
    pub script_count: usize,
    pub expected_script_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRootPreferences {
    pub schema_version: u32,
    pub roots: BTreeMap<String, PathBuf>,
    pub android_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderConfig {
    pub id: String,
    pub display_name: String,
    pub protocol: String,
    pub base_url: String,
    pub selected_model: Option<String>,
    pub api_key_configured: bool,
    pub builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    pub schema_version: u32,
    pub active_provider_id: Option<String>,
    pub providers: Vec<AiProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderInput {
    pub id: String,
    pub display_name: String,
    pub protocol: String,
    pub base_url: String,
    pub selected_model: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiModelInfo {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiDiagnosticAnalysis {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub issue_code: String,
    pub generated_at: DateTime<Utc>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCommand {
    pub label: String,
    pub shell: String,
    pub command: String,
    pub changes_environment: bool,
    pub requires_elevation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MachineContext {
    pub platform: String,
    pub process_architecture: String,
    pub windows_architecture: String,
    pub data_root: PathBuf,
    pub configured_tool_roots: BTreeMap<String, PathBuf>,
    pub user_environment_variable_count: usize,
    pub system_environment_variable_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticGuidance {
    pub issue_code: String,
    pub analysis_source: String,
    pub summary: String,
    pub root_causes: Vec<String>,
    pub machine_factors: Vec<String>,
    pub recommendations: Vec<String>,
    pub commands: Vec<DiagnosticCommand>,
    pub one_click_available: bool,
    pub one_click_label: Option<String>,
    pub requires_elevation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReport {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub machine: MachineContext,
    pub scan: EnvironmentScan,
    pub guidance: Vec<DiagnosticGuidance>,
}

impl Default for ToolRootPreferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            roots: BTreeMap::new(),
            android_root: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentBackupSummary {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub operation_id: String,
    pub variable_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationLogEntry {
    pub timestamp: DateTime<Utc>,
    pub operation_id: String,
    pub level: String,
    pub event: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentScope {
    User,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDiff {
    pub scope: EnvironmentScope,
    pub variable: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub kind: String,
    pub description: String,
    pub destructive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationPlan {
    pub id: String,
    pub tool_id: String,
    pub title: String,
    pub summary: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub confirmation_token: String,
    pub requires_elevation: bool,
    pub warnings: Vec<String>,
    pub conflicts: Vec<DiagnosticIssue>,
    pub environment_diffs: Vec<EnvironmentDiff>,
    pub steps: Vec<PlanStep>,
    #[serde(skip)]
    pub environment_fingerprint: String,
    #[serde(skip)]
    pub action: PlannedAction,
}

#[derive(Debug, Clone, Default)]
pub enum PlannedAction {
    Switch {
        tool_id: String,
        installation_path: PathBuf,
    },
    Install(InstallRequest),
    Repair(InstallRequest),
    Uninstall {
        tool_id: String,
        installation_path: PathBuf,
        user_environment_after: Option<BTreeMap<String, String>>,
    },
    RestoreEnvironment {
        backup_path: PathBuf,
    },
    UpdateUserEnvironment {
        updated: BTreeMap<String, String>,
        reason: String,
    },
    #[default]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest {
    pub tool_id: String,
    pub version: String,
    pub root: PathBuf,
    pub destination: PathBuf,
    pub download_url: String,
    pub checksum_algorithm: Option<String>,
    pub checksum: Option<String>,
}
