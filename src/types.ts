export type ThemeId =
  | "modern-tech"
  | "cyberpunk"
  | "japanese-light"
  | "game-hud"
  | "professional-minimal";

export type HealthLevel = "healthy" | "warning" | "error" | "unknown";

export interface InstalledVersion {
  version: string;
  path: string;
  source: string;
  isDefault: boolean;
  managed: boolean;
  health: HealthLevel;
  executable?: string;
}

export interface DiagnosticIssue {
  code: string;
  level: "info" | "warning" | "error";
  title: string;
  detail: string;
  evidence?: string;
  repairable: boolean;
}

export interface DiagnosticCommand {
  label: string;
  shell: string;
  command: string;
  changesEnvironment: boolean;
  requiresElevation: boolean;
}

export interface DiagnosticGuidance {
  issueCode: string;
  analysisSource: string;
  summary: string;
  rootCauses: string[];
  machineFactors: string[];
  recommendations: string[];
  commands: DiagnosticCommand[];
  oneClickAvailable: boolean;
  oneClickLabel?: string;
  requiresElevation: boolean;
}

export interface ToolInventory {
  id: string;
  displayName: string;
  category: string;
  icon: string;
  capabilities: {
    install: boolean;
    switchDefault: boolean;
    repair: boolean;
    uninstall: boolean;
  };
  defaultVersion?: InstalledVersion;
  installedVersions: InstalledVersion[];
  environmentStatus: HealthLevel;
  issues: DiagnosticIssue[];
  scannedAt: string;
}

export interface ToolDefinition {
  id: string;
  displayName: string;
  category: string;
  icon: string;
  capabilities: ToolInventory["capabilities"];
}

export interface EnvironmentScan {
  tools: ToolInventory[];
  issues: DiagnosticIssue[];
  versionManagers: VersionManagerInventory[];
  userPathEntries: number;
  scanStartedAt: string;
  scanFinishedAt: string;
}

export interface VersionManagerInventory {
  id: string;
  displayName: string;
  toolIds: string[];
  executable?: string;
  root?: string;
  currentVersion?: string;
  evidence: string;
}

export interface RemoteVersion {
  version: string;
  channel: string;
  publishedAt?: string;
  architecture: string;
  downloadUrl?: string;
  checksumAlgorithm?: "sha512" | "sha256" | "sha1";
  checksum?: string;
  notesUrl?: string;
}

export interface VersionCatalog {
  toolId: string;
  sourceName: string;
  sourceUrl: string;
  fetchedAt: string;
  cached: boolean;
  versions: RemoteVersion[];
}

export interface BootstrapState {
  appVersion: string;
  dataRoot: string;
  configReady: boolean;
  platform: string;
}

export type CloseBehavior = "minimizeToTray" | "exit";
export type AppLanguage = "zh-CN" | "zh-TW" | "en-US" | "ja-JP" | "ko-KR";

export interface AppPreferences {
  schemaVersion: number;
  closeBehavior: CloseBehavior;
  startMinimized: boolean;
  launchAtLogin: boolean;
  language: AppLanguage;
}

export type TrayAction =
  | { kind: "navigate"; view: "dashboard" | "tools" | "diagnostics" | "settings" }
  | { kind: "scan" }
  | { kind: "openTool"; toolId: string }
  | { kind: "selectAiProvider"; providerId: string }
  | { kind: "previewSwitch"; toolId: string; installationPath: string }
  | { kind: "openDiagnostic"; issueCode: string }
  | { kind: "previewDiagnosticRepair"; issueCode: string };

export interface TerminalCommandStatus {
  directory: string;
  enabledInUserPath: boolean;
  scriptCount: number;
  expectedScriptCount: number;
}

export interface ToolRootPreferences {
  schemaVersion: number;
  roots: Record<string, string>;
  androidRoot?: string;
}

export type AiProtocol = "openai" | "anthropic" | "gemini";

export interface AiProviderConfig {
  id: string;
  displayName: string;
  protocol: AiProtocol;
  baseUrl: string;
  selectedModel?: string;
  apiKeyConfigured: boolean;
  builtin: boolean;
}

export interface AiSettings {
  schemaVersion: number;
  activeProviderId?: string;
  providers: AiProviderConfig[];
}

export interface AiProviderInput {
  id: string;
  displayName: string;
  protocol: AiProtocol;
  baseUrl: string;
  selectedModel?: string;
  apiKey?: string;
}

export interface AiModelInfo {
  id: string;
  displayName: string;
}

export interface AiDiagnosticAnalysis {
  providerId: string;
  providerName: string;
  model: string;
  issueCode: string;
  generatedAt: string;
  content: string;
}

export interface EnvironmentDiff {
  scope: "user" | "system";
  variable: string;
  before?: string;
  after?: string;
  added: string[];
  removed: string[];
}

export interface EnvironmentBackupSummary {
  id: string;
  createdAt: string;
  operationId: string;
  variableCount: number;
}

export interface OperationLogEntry {
  timestamp: string;
  operationId: string;
  level: string;
  event: string;
  path: string;
}

export interface OperationPlan {
  id: string;
  toolId: string;
  title: string;
  summary: string;
  createdAt: string;
  expiresAt: string;
  confirmationToken: string;
  requiresElevation: boolean;
  warnings: string[];
  conflicts: DiagnosticIssue[];
  environmentDiffs: EnvironmentDiff[];
  steps: Array<{
    kind: string;
    description: string;
    destructive: boolean;
  }>;
}

export interface OperationProgress {
  operationId: string;
  phase: string;
  message: string;
  receivedBytes: number;
  totalBytes?: number;
  percent?: number;
}

export interface OperationResult {
  operationId: string;
  status: string;
  message: string;
  installationPath?: string;
}
