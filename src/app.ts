import { createElement, icons } from "lucide";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import {
  readMainScrollPosition,
  restoreMainScrollPosition,
} from "./scroll-state";
import { backend } from "./bridge";
import { aiProviderIcon } from "./ai-icons";
import { toolBrandIcon } from "./brand-icons";
import { localizeUi } from "./i18n";
import {
  getStoredNavigation,
  storeNavigation,
  type ViewId,
} from "./navigation-state";
import { applyTheme, getStoredTheme, themes } from "./theme";
import type {
  AiDiagnosticAnalysis,
  AiModelInfo,
  AiProviderConfig,
  AiProviderInput,
  AiSettings,
  AppLanguage,
  AppPreferences,
  BootstrapState,
  DiagnosticGuidance,
  DiagnosticIssue,
  EnvironmentBackupSummary,
  EnvironmentScan,
  HealthLevel,
  OperationLogEntry,
  OperationPlan,
  OperationProgress,
  ToolDefinition,
  ToolInventory,
  TrayAction,
  TerminalCommandStatus,
  VersionCatalog,
} from "./types";

type ToolView = Omit<ToolInventory, "scannedAt"> & {
  scannedAt?: string;
  scanned: boolean;
};

type ApplicationUpdateStatus = {
  phase: "checking" | "current" | "available" | "downloading" | "error";
  message: string;
  availableVersion?: string;
  notes?: string;
  progressPercent?: number;
};

const navItems: Array<{ id: ViewId; label: string; icon: keyof typeof icons }> = [
  { id: "dashboard", label: "总览", icon: "LayoutDashboard" },
  { id: "tools", label: "工具链", icon: "Blocks" },
  { id: "changes", label: "变更中心", icon: "GitCompareArrows" },
  { id: "diagnostics", label: "诊断与日志", icon: "Activity" },
  { id: "commands", label: "命令说明", icon: "SquareTerminal" },
  { id: "settings", label: "设置", icon: "Settings2" },
];

const storedNavigation = getStoredNavigation();

const state: {
  view: ViewId;
  bootstrap?: BootstrapState;
  appPreferences?: AppPreferences;
  trayReady: boolean;
  toolDefinitions: ToolDefinition[];
  scan?: EnvironmentScan;
  catalogs: Map<string, VersionCatalog>;
  scanning: boolean;
  error?: string;
  selectedToolId?: string;
  pendingPlan?: OperationPlan;
  applying: boolean;
  progress?: OperationProgress;
  notice?: string;
  backups: EnvironmentBackupSummary[];
  logs: OperationLogEntry[];
  androidRoot?: string;
  toolRoots: Record<string, string>;
  scanStale: boolean;
  aiSettings?: AiSettings;
  selectedAiProviderId?: string;
  aiModels: AiModelInfo[];
  aiAnalysis?: AiDiagnosticAnalysis;
  diagnosticGuidance?: DiagnosticGuidance;
  aiAnalyzing: boolean;
  terminalCommands?: TerminalCommandStatus;
  applicationUpdate?: ApplicationUpdateStatus;
  toolSearchQuery: string;
  toolFilter: string;
} = {
  view: storedNavigation.view,
  trayReady: false,
  toolDefinitions: [],
  catalogs: new Map(),
  scanning: false,
  applying: false,
  backups: [],
  logs: [],
  toolRoots: {},
  scanStale: false,
  aiModels: [],
  aiAnalyzing: false,
  selectedToolId: storedNavigation.selectedToolId,
  selectedAiProviderId: storedNavigation.selectedAiProviderId,
  toolSearchQuery: "",
  toolFilter: "all",
};

let toolContextPromise: Promise<void> | undefined;
let pendingApplicationUpdate: Update | undefined;
let lastRenderedView: ViewId | undefined;

function ensureToolContext(): Promise<void> {
  if (!toolContextPromise) {
    toolContextPromise = Promise.all([
      backend.toolDefinitions(),
      backend.toolRootPreferences(),
      backend.cachedEnvironmentScan(),
    ])
      .then(([definitions, preferences, cachedScan]) => {
        state.toolDefinitions = definitions;
        state.androidRoot = preferences.androidRoot;
        state.toolRoots = preferences.roots;
        state.scan = cachedScan ?? undefined;
      })
      .catch((error) => {
        toolContextPromise = undefined;
        throw error;
      });
  }
  return toolContextPromise;
}

const androidWorkspaceTools = new Set([
  "android-sdk",
  "android-ndk",
  "adb",
  "java",
  "gradle",
  "cmake",
]);

function icon(name: keyof typeof icons, size = 18): string {
  const node = createElement(icons[name], {
    width: size,
    height: size,
    "stroke-width": 1.8,
  });
  return node.outerHTML;
}

function renderToolBrand(toolId: string, fallback: string, size = 26): string {
  return toolBrandIcon(toolId, size) || escapeHtml(fallback);
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function healthLabel(level: HealthLevel): string {
  return {
    healthy: "正常",
    warning: "需关注",
    error: "异常",
    unknown: "未检测",
  }[level];
}

function healthScore(scan?: EnvironmentScan): number {
  if (!scan || scan.tools.length === 0) return 0;
  const weights: Record<HealthLevel, number> = {
    healthy: 100,
    warning: 64,
    error: 22,
    unknown: 45,
  };
  return Math.round(
    scan.tools.reduce((sum, tool) => sum + weights[tool.environmentStatus], 0) /
      scan.tools.length,
  );
}

function managedTools(): ToolView[] {
  const scannedById = new Map(
    (state.scan?.tools ?? []).map((tool) => [tool.id, tool] as const),
  );
  return state.toolDefinitions.map((definition) => {
    const scanned = scannedById.get(definition.id);
    if (scanned) return { ...scanned, scanned: true };
    return {
      ...definition,
      defaultVersion: undefined,
      installedVersions: [],
      environmentStatus: "unknown",
      issues: [],
      scannedAt: undefined,
      scanned: false,
    };
  });
}

function renderShell(): string {
  const active = state.view;
  const activeNav = active === "tool-detail" ? "tools" : active;
  const selectedTool =
    active === "tool-detail"
      ? managedTools().find((tool) => tool.id === state.selectedToolId)
      : undefined;
  const scan = state.scan;
  const diagnosticCount =
    (scan?.issues.length ?? 0) +
    (scan?.tools.reduce((total, tool) => total + tool.issues.length, 0) ?? 0);
  const score = healthScore(scan);
  return `
    <div class="app-shell">
      <aside class="sidebar">
        <div class="brand">
          <div class="brand-mark"><span></span><span></span><span></span></div>
          <div>
            <strong>EnvNexus AI</strong>
            <small>开发环境控制台</small>
          </div>
        </div>
        <nav class="primary-nav">
          ${navItems
            .map(
              (item) => `
                <button class="nav-item ${activeNav === item.id ? "active" : ""}" data-nav="${item.id}">
                  ${icon(item.icon)}
                  <span>${item.label}</span>
                  ${item.id === "diagnostics" && diagnosticCount ? `<em>${diagnosticCount}</em>` : ""}
                </button>
              `,
            )
            .join("")}
        </nav>
        <div class="sidebar-spacer"></div>
        <div class="safety-card">
          <div class="safety-icon">${icon("ShieldCheck", 19)}</div>
          <div>
            <strong>安全模式已开启</strong>
            <span>仅允许用户级变更</span>
          </div>
        </div>
        <div class="data-root">
          <span>数据目录</span>
          <code title="${escapeHtml(state.bootstrap?.dataRoot ?? "正在读取…")}">${escapeHtml(
            state.bootstrap?.dataRoot ?? "正在读取…",
          )}</code>
        </div>
      </aside>
      <main class="main">
        <header class="topbar">
          <div class="breadcrumb">
            <span>工作区</span>
            ${icon("ChevronRight", 14)}
            ${active === "tool-detail" ? `<button data-nav="tools">工具链</button>${icon("ChevronRight", 14)}` : ""}
            <strong>${selectedTool ? escapeHtml(selectedTool.displayName) : (navItems.find((item) => item.id === active)?.label ?? "")}</strong>
          </div>
          <div class="top-actions">
            <button class="scan-button" id="scan-button" ${state.scanning ? "disabled" : ""}>
              ${icon(state.scanning ? "LoaderCircle" : "Radar")}
              ${state.scanning ? "正在扫描" : state.scan ? "重新扫描" : "开始扫描"}
            </button>
          </div>
        </header>
        <section class="content">
          ${state.error ? `<div class="error-banner">${icon("CircleAlert")}<span>${escapeHtml(state.error)}</span></div>` : ""}
          ${state.notice ? `<div class="notice-banner">${icon("CircleCheck")}<span>${escapeHtml(state.notice)}</span><button id="dismiss-notice">${icon("X", 14)}</button></div>` : ""}
          ${renderView(score)}
        </section>
        ${renderOverlays()}
      </main>
    </div>
  `;
}

function renderView(score: number): string {
  switch (state.view) {
    case "tools":
      return renderTools();
    case "tool-detail":
      return renderToolDetail();
    case "changes":
      return renderChanges();
    case "diagnostics":
      return renderDiagnostics();
    case "commands":
      return renderCommands();
    case "settings":
      return renderSettings();
    default:
      return renderDashboard(score);
  }
}

function renderDashboard(score: number): string {
  const scan = state.scan;
  const tools = scan?.tools ?? [];
  const installed = tools.filter((tool) => tool.installedVersions.length > 0).length;
  const warnings = tools.reduce(
    (sum, tool) => sum + tool.issues.filter((issue) => issue.level !== "info").length,
    0,
  );
  return `
    <div class="page-heading">
      <div>
        <p class="eyebrow">ENVIRONMENT OVERVIEW</p>
        <h1>开发环境，一目了然</h1>
        <p>检测冲突、安全切换版本，并让每一次变更都可预览、可恢复。</p>
      </div>
      <div class="scan-time">
        ${icon("Clock3", 16)}
        <span>${
          scan
            ? `上次扫描 ${new Date(scan.scanFinishedAt).toLocaleString(state.appPreferences?.language ?? "zh-CN")}${state.scanStale ? " · 环境已变更，等待手动重扫" : ""}`
            : "尚未扫描；EnvNexus AI 不会在启动时自动扫描"
        }</span>
      </div>
    </div>
    <div class="dashboard-grid">
      <article class="health-panel panel">
        <div class="panel-label"><span>环境健康度</span><i class="live-dot"></i></div>
        <div class="score-wrap">
          <div class="score-ring" style="--score:${score}">
            <div><strong>${state.scanning ? "…" : score}</strong><small>/ 100</small></div>
          </div>
          <div class="score-copy">
            <h2>${score >= 80 ? "整体状态良好" : score > 0 ? "发现可优化项" : "等待首次扫描"}</h2>
            <p>${score > 0 ? `${warnings} 个问题需要确认，未对系统执行任何修改。` : "点击“开始扫描”读取本机实际环境；启动 App 不会自动扫描。"}</p>
            <button class="text-button" data-nav="diagnostics">查看诊断报告 ${icon("ArrowUpRight", 15)}</button>
          </div>
        </div>
        <div class="metrics">
          <div><span>已识别工具</span><strong>${installed}<small> / ${tools.length || 15}</small></strong></div>
          <div><span>环境冲突</span><strong>${warnings}</strong></div>
          <div><span>待处理变更</span><strong>0</strong></div>
        </div>
      </article>
      <article class="quick-panel panel">
        <div class="panel-title">
          <div><p class="eyebrow">QUICK ACTIONS</p><h2>快捷操作</h2></div>
          ${icon("Zap", 19)}
        </div>
        <button class="quick-action" data-nav="tools">
          <span class="quick-icon blue">${icon("PackagePlus")}</span>
          <span><strong>安装新工具链</strong><small>从官方源选择版本</small></span>
          ${icon("ChevronRight")}
        </button>
        <button class="quick-action" id="quick-scan">
          <span class="quick-icon violet">${icon("ScanSearch")}</span>
          <span><strong>运行环境诊断</strong><small>只读扫描，不做修改</small></span>
          ${icon("ChevronRight")}
        </button>
        <button class="quick-action" data-nav="changes">
          <span class="quick-icon amber">${icon("History")}</span>
          <span><strong>恢复环境备份</strong><small>预览后再应用</small></span>
          ${icon("ChevronRight")}
        </button>
      </article>
    </div>
    <div class="section-heading">
      <div><p class="eyebrow">TOOLCHAINS</p><h2>默认工具链</h2></div>
      <button class="text-button" data-nav="tools">管理全部 ${icon("ArrowRight", 15)}</button>
    </div>
    <div class="tool-grid">
      ${state.scanning && tools.length === 0 ? renderToolSkeletons() : tools.slice(0, 8).map(renderToolCard).join("")}
      ${!state.scanning && tools.length === 0 ? renderEmptyScan() : ""}
    </div>
    ${renderIssueStrip(scan?.issues.slice(0, 3) ?? [])}
  `;
}

function renderToolSkeletons(): string {
  return Array.from(
    { length: 8 },
    () => `<div class="tool-card panel skeleton"><i></i><p></p><span></span></div>`,
  ).join("");
}

function renderEmptyScan(): string {
  return `
    <div class="empty-card panel">
      ${icon("Radar", 30)}
      <strong>尚无扫描结果</strong>
      <span>启动只读扫描后，这里会显示真实的本机工具链。</span>
      <button class="primary-button" id="empty-scan">开始扫描</button>
    </div>
  `;
}

function renderToolCard(tool: ToolInventory): string {
  const current = tool.defaultVersion;
  return `
    <article class="tool-card panel" data-open-tool="${escapeHtml(tool.id)}">
      <div class="tool-card-top">
        <div class="tool-logo tool-${escapeHtml(tool.id)}">${renderToolBrand(tool.id, tool.icon)}</div>
        <span class="status ${tool.environmentStatus}"><i></i>${healthLabel(tool.environmentStatus)}</span>
      </div>
      <div class="tool-name">
        <h3>${escapeHtml(tool.displayName)}</h3>
        <span>${escapeHtml(tool.category)}</span>
      </div>
      <div class="version-row">
        <span>默认版本</span>
        <strong>${current ? escapeHtml(current.version) : "未安装"}</strong>
      </div>
      <div class="tool-location" title="${escapeHtml(current?.path ?? "未发现路径")}">
        ${icon("FolderClosed", 14)}
        <code>${escapeHtml(current?.path ?? "未发现路径")}</code>
      </div>
      <div class="tool-footer">
        <span>${tool.installedVersions.length} 个已安装版本</span>
        <button class="icon-button small" aria-label="打开详情">${icon("ArrowUpRight", 16)}</button>
      </div>
    </article>
  `;
}

function renderIssueStrip(issues: DiagnosticIssue[]): string {
  if (issues.length === 0) return "";
  return `
    <div class="section-heading issue-heading">
      <div><p class="eyebrow">ATTENTION</p><h2>需要关注</h2></div>
      <button class="text-button" data-nav="diagnostics">全部问题 ${icon("ArrowRight", 15)}</button>
    </div>
    <div class="issue-list">
      ${issues
        .map(
          (issue) => `
            <article class="issue ${issue.level}">
              <span>${icon(issue.level === "error" ? "OctagonAlert" : "TriangleAlert", 18)}</span>
              <div><strong>${escapeHtml(issue.title)}</strong><p>${escapeHtml(issue.detail)}</p></div>
              <button data-nav="diagnostics">查看</button>
            </article>
          `,
        )
        .join("")}
    </div>
  `;
}

function renderTools(): string {
  const tools = managedTools();
  const coreIds = new Set([
    "python",
    "go",
    "rust",
    "node",
    "git",
    "maven",
    "dotnet",
    "ruby",
    "php",
  ]);
  const coreTools = tools.filter((tool) => coreIds.has(tool.id));
  const androidTools = tools.filter((tool) => androidWorkspaceTools.has(tool.id));
  const remainingTools = tools.filter(
    (tool) => !coreIds.has(tool.id) && !androidWorkspaceTools.has(tool.id),
  );
  return `
    <div class="page-heading compact">
      <div><p class="eyebrow">TOOLCHAIN LIBRARY</p><h1>工具链</h1><p>无需扫描即可进入每个工具的独立管理页、填写安装目录和查询官方版本；扫描只负责读取本机状态。</p></div>
      <button class="secondary-button" id="tools-rescan">${icon("Radar", 17)} ${state.scan ? "重新扫描" : "扫描本机状态"}</button>
    </div>
    <div class="toolbar panel">
      <label class="search-field">${icon("Search", 17)}<input id="tool-search" placeholder="搜索工具…" value="${escapeHtml(state.toolSearchQuery)}" /></label>
      <div class="filter-pills">${(
        [
          ["all", "全部"],
          ["installed", "已安装"],
          ["issues", "有问题"],
        ] as const
      )
        .map(
          ([filterId, label]) =>
            `<button class="${state.toolFilter === filterId ? "active" : ""}" data-tool-filter="${filterId}">${label}</button>`,
        )
        .join("")}</div>
    </div>
    ${
      tools.length
        ? `
          ${renderToolGroup("常用开发工具", "语言运行时、编译工具链和版本控制", "core", coreTools)}
          ${renderToolGroup("Android 构建工具链", "SDK、NDK、JDK、Gradle、CMake 与 ADB 共用一个根目录", "android", androidTools, true)}
          ${remainingTools.length ? renderToolGroup("其他工具", "已注册的扩展工具", "other", remainingTools) : ""}
        `
        : `<div class="list-empty panel">正在读取内置工具目录…</div>`
    }
  `;
}

function renderToolGroup(
  title: string,
  description: string,
  groupId: string,
  tools: ToolView[],
  android = false,
): string {
  return `
    <section class="tool-group" data-tool-group="${groupId}">
      <header class="tool-group-heading">
        <div><p class="eyebrow">${groupId === "android" ? "ANDROID STACK" : "DEVELOPMENT TOOLS"}</p><h2>${title}</h2><p>${description}</p></div>
        <span>${tools.length} 个工具</span>
      </header>
      ${
        android
          ? `<div class="android-root-inline panel">
               <div class="path-icon">${icon("FolderCog", 21)}</div>
               <div><span>Android 统一安装根目录</span><code>${escapeHtml(state.androidRoot ?? "尚未设置")}</code><small>可直接填写或浏览选择；保存后同步到 SDK、NDK、JDK、Gradle、CMake、ADB。</small></div>
               <div class="inline-root-actions">
                 <input data-android-root-input value="${escapeHtml(state.androidRoot ?? "")}" placeholder="例如 E:\\Development\\Android" spellcheck="false">
                 <button class="secondary-button" data-save-android-root>保存路径</button>
                 <button class="secondary-button" data-select-android-root>浏览</button>
               </div>
             </div>`
          : ""
      }
      <div class="tool-library-grid">
        ${tools.map(renderToolLibraryCard).join("")}
      </div>
    </section>
  `;
}

function renderToolLibraryCard(tool: ToolView): string {
  const root = state.toolRoots[tool.id];
  return `
    <button class="tool-library-card panel" data-open-tool="${escapeHtml(tool.id)}" data-tool-card
      data-search="${escapeHtml(`${tool.displayName} ${tool.id} ${tool.category}`.toLocaleLowerCase())}"
      data-installed="${tool.installedVersions.length > 0}" data-issues="${tool.issues.length > 0}">
      <span class="tool-library-top">
        <span class="tool-logo tool-${escapeHtml(tool.id)}">${renderToolBrand(tool.id, tool.icon)}</span>
        <span class="status ${tool.environmentStatus}"><i></i>${healthLabel(tool.environmentStatus)}</span>
      </span>
      <span class="tool-library-copy"><strong>${escapeHtml(tool.displayName)}</strong><small>${escapeHtml(tool.category)}</small></span>
      <span class="tool-library-stats">
        <span><small>当前默认</small><strong>${escapeHtml(tool.scanned ? (tool.defaultVersion?.version ?? "未安装") : "未扫描")}</strong></span>
        <span><small>本机版本</small><strong>${tool.installedVersions.length}</strong></span>
      </span>
      <span class="tool-library-path ${root ? "" : "unset"}">${icon("Folder", 14)}<code>${escapeHtml(root ?? "未设置安装目录")}</code></span>
      <span class="tool-library-open">进入独立管理页 ${icon("ArrowRight", 15)}</span>
    </button>
  `;
}

function renderToolDetail(): string {
  const tool = managedTools().find((candidate) => candidate.id === state.selectedToolId);
  if (!tool) {
    return `
      <div class="empty-state panel">
        ${icon("Blocks", 34)}
        <h2>工具信息不可用</h2>
        <p>内置工具目录尚未加载，请稍后返回工具链重试。</p>
        <button class="primary-button" data-nav="tools">返回工具链</button>
      </div>
    `;
  }
  const catalog = state.catalogs.get(tool.id);
  const installRoot = state.toolRoots[tool.id];
  const sharedAndroidRoot = androidWorkspaceTools.has(tool.id);
  return `
    <div class="tool-detail-page">
      <button class="back-button" data-nav="tools">${icon("ArrowLeft", 16)} 返回工具链</button>
      <section class="tool-detail-hero panel">
        <div class="tool-detail-identity">
          <span class="tool-logo tool-${escapeHtml(tool.id)}">${renderToolBrand(tool.id, tool.icon, 38)}</span>
          <div><p class="eyebrow">${escapeHtml(tool.id.toLocaleUpperCase())}</p><h1>${escapeHtml(tool.displayName)}</h1><p>${escapeHtml(tool.category)} · 独立工具管理页</p></div>
        </div>
        <div class="tool-detail-summary">
          <span><small>环境状态</small><strong class="status ${tool.environmentStatus}"><i></i>${healthLabel(tool.environmentStatus)}</strong></span>
          <span><small>当前默认</small><strong>${escapeHtml(tool.scanned ? (tool.defaultVersion?.version ?? "未安装") : "未扫描")}</strong></span>
          <span><small>已安装版本</small><strong>${tool.installedVersions.length}</strong></span>
        </div>
      </section>
      <section class="install-root-panel panel ${installRoot ? "configured" : "missing"}">
        <div class="path-icon">${icon(sharedAndroidRoot ? "Boxes" : "FolderCog", 23)}</div>
        <div>
          <p class="eyebrow">${sharedAndroidRoot ? "SHARED ANDROID ROOT" : "DEFAULT INSTALL ROOT"}</p>
          <h2>${sharedAndroidRoot ? "Android 共用安装根目录" : `${escapeHtml(tool.displayName)} 默认安装根目录`}</h2>
          <code>${escapeHtml(installRoot ?? "尚未设置；设置后才能安装官方版本")}</code>
          <p>${sharedAndroidRoot ? "该目录同时用于 SDK、NDK、JDK、Gradle、CMake 与 ADB，修改后会同步到整个 Android 分组。" : "选择结果保存在 EnvNexus AI 数据目录中；以后安装该工具的新版本会自动使用此根目录。"}</p>
        </div>
        <div class="install-root-actions">
          <input data-tool-root-input="${escapeHtml(tool.id)}" value="${escapeHtml(installRoot ?? "")}" placeholder="例如 E:\\Development\\${escapeHtml(tool.id)}" spellcheck="false">
          <button class="${installRoot ? "secondary-button" : "primary-button"}" data-save-tool-root="${escapeHtml(tool.id)}">${icon("Save", 16)} 保存路径</button>
          <button class="secondary-button" data-select-tool-root="${escapeHtml(tool.id)}">${icon("FolderOpen", 16)} 浏览</button>
        </div>
      </section>
      <div class="tool-detail-columns">
        <section class="tool-detail-section panel">
          <div class="drawer-section-title"><div><p class="eyebrow">INSTALLED VERSIONS</p><h3>本机已安装版本</h3></div><span>${tool.installedVersions.length}</span></div>
          <div class="version-stack">
            ${
              tool.installedVersions.length
                ? tool.installedVersions
                    .map(
                      (version) => `
                        <article class="version-item ${version.isDefault ? "default" : ""}">
                          <div><strong>${escapeHtml(version.version)}</strong><span>${version.isDefault ? "当前默认" : version.managed ? "EnvNexus AI 受管" : escapeHtml(version.source)}</span><code title="${escapeHtml(version.path)}">${escapeHtml(version.path)}</code></div>
                          <div class="version-actions">
                            ${!version.isDefault ? `<button class="secondary-button" data-switch-path="${encodeURIComponent(version.path)}">切换默认</button>` : ""}
                            ${version.managed ? `<button class="secondary-button" data-repair-path="${encodeURIComponent(version.path)}">修复</button>` : ""}
                            ${version.managed ? `<button class="danger-button" data-uninstall-path="${encodeURIComponent(version.path)}">卸载</button>` : ""}
                          </div>
                        </article>
                      `,
                    )
                    .join("")
                : `<div class="drawer-empty">${icon("HardDrive", 26)}<span>${tool.scanned ? "本次扫描没有发现该工具的已安装版本。" : "尚未扫描本机状态；仍可在上方设置目录并在右侧查询官方版本。"}</span></div>`
            }
          </div>
        </section>
        <section class="tool-detail-section panel">
          <div class="drawer-section-title">
            <div><p class="eyebrow">OFFICIAL RELEASES</p><h3>官方可安装版本</h3></div>
            <button class="text-button" data-fetch-versions="${escapeHtml(tool.id)}">${catalog ? "刷新版本" : "查询官方源"}</button>
          </div>
          ${
            catalog
              ? `<div class="source-proof">${icon("BadgeCheck", 15)}<span>${escapeHtml(catalog.sourceName)}</span><time>${new Date(catalog.fetchedAt).toLocaleString(state.appPreferences?.language ?? "zh-CN")}</time></div>
                 <div class="version-stack remote-stack">
                   ${catalog.versions
                     .slice(0, 30)
                     .map(
                       (version) => `
                         <article class="version-item">
                           <div><strong>${escapeHtml(version.version)}</strong><span>${escapeHtml(version.channel)} · ${escapeHtml(version.architecture)}</span><code>${version.checksum ? `${escapeHtml(version.checksumAlgorithm ?? "")}: ${escapeHtml(version.checksum.slice(0, 16))}…` : "官方源未提供校验值"}</code></div>
                           <button class="primary-button" data-install-version="${encodeURIComponent(version.version)}" ${installRoot ? "" : "disabled"} title="${installRoot ? "生成安装计划" : "请先在页面上方设置安装目录"}">${icon("Download", 15)} ${installRoot ? "安装" : "先设置目录"}</button>
                         </article>
                       `,
                     )
                     .join("")}
                 </div>`
              : `<div class="drawer-empty">${icon("CloudDownload", 28)}<span>点击“查询官方源”获取实时版本；安装前请先设置页面上方的默认安装目录。</span></div>`
          }
        </section>
      </div>
    </div>
  `;
}

function renderChanges(): string {
  return `
    <div class="page-heading compact"><div><p class="eyebrow">CHANGE CENTER</p><h1>变更中心</h1><p>所有持久化操作都先生成差异，再由你明确确认。</p></div></div>
    <div class="change-layout">
      <div class="empty-state panel">
        <span class="empty-graphic">${icon("GitCompareArrows", 38)}</span>
        <h2>当前没有待确认变更</h2>
        <p>安装、切换版本或修复环境时，PATH 和环境变量差异会出现在这里。</p>
        <div class="flow-steps"><span>生成计划</span>${icon("ArrowRight", 15)}<span>差异预览</span>${icon("ArrowRight", 15)}<span>备份</span>${icon("ArrowRight", 15)}<span>执行与验证</span></div>
      </div>
      <aside class="panel backup-panel">
        <div class="drawer-section-title"><div><p class="eyebrow">BACKUPS</p><h3>环境备份</h3></div><button class="text-button" id="refresh-backups">刷新</button></div>
        <p class="backup-note">${icon("ShieldCheck", 14)} 每次用户环境写入前自动创建</p>
        <div class="backup-list">
          ${
            state.backups.length
              ? state.backups
                  .slice(0, 12)
                  .map(
                    (backup) => `
                      <article>
                        <div><strong>${new Date(backup.createdAt).toLocaleString(state.appPreferences?.language ?? "zh-CN")}</strong><span>${backup.variableCount} 个用户变量</span><code>${escapeHtml(backup.operationId.slice(0, 12))}</code></div>
                        <button class="secondary-button" data-restore-backup="${encodeURIComponent(backup.id)}">预览恢复</button>
                      </article>`,
                  )
                  .join("")
              : `<div class="drawer-empty">${icon("ArchiveRestore", 26)}<span>尚无环境备份；未执行过持久化修改是正常状态。</span></div>`
          }
        </div>
      </aside>
    </div>
  `;
}

function renderDiagnostics(): string {
  const issues = [
    ...(state.scan?.issues ?? []),
    ...(state.scan?.tools.flatMap((tool) => tool.issues) ?? []),
  ];
  const managers = state.scan?.versionManagers ?? [];
  const activeAi = activeAiProvider();
  return `
    <div class="page-heading compact"><div><p class="eyebrow">DIAGNOSTICS</p><h1>诊断与日志</h1><p>本地规则引擎始终分析版本、PATH、环境变量和版本管理器；配置 AI 后可在本地安全结论之上增强分析。</p></div><button class="secondary-button" id="export-diagnostics" ${state.scan ? "" : "disabled"}>${icon("FileDown", 17)} 导出完整报告</button></div>
    <section class="panel manager-awareness">
      <div class="panel-title">
        <div><p class="eyebrow">VERSION MANAGERS</p><h2>版本管理器感知</h2></div>
        <span class="manager-count">${managers.length} 个已识别</span>
      </div>
      ${
        managers.length
          ? `<div class="manager-grid">${managers
              .map(
                (manager) => `
                  <article>
                    <span>${icon("Waypoints", 18)}</span>
                    <div><strong>${escapeHtml(manager.displayName)}</strong><p>管理 ${manager.toolIds.map(escapeHtml).join(" / ")}${manager.currentVersion ? ` · 当前 ${escapeHtml(manager.currentVersion)}` : ""}</p><code>${escapeHtml(manager.evidence)}</code></div>
                  </article>`,
              )
              .join("")}</div>`
          : `<div class="manager-empty">${state.scan ? "本次扫描未发现 pyenv、NVM、fnm、Volta、rustup、Jabba 或 goenv。" : "尚未手动扫描，暂时没有版本管理器信息。"}</div>`
      }
    </section>
    <div class="diagnostic-layout">
      <section class="panel diagnostic-list">
        <div class="panel-title"><div><p class="eyebrow">FINDINGS</p><h2>${issues.length} 项发现</h2></div>${activeAi?.apiKeyConfigured && activeAi.selectedModel ? `<span class="ai-active">${icon("Sparkles", 14)} 本地规则 + ${escapeHtml(activeAi.displayName)} / ${escapeHtml(activeAi.selectedModel)}</span>` : `<span class="ai-active">${icon("ShieldCheck", 14)} EnvNexus AI 本地规则引擎</span>`}</div>
        ${issues.length ? issues.map(renderDiagnostic).join("") : `<div class="list-empty">尚无诊断结果。</div>`}
      </section>
      <aside class="panel log-preview">
        <p class="eyebrow">OPERATION LOG</p><h2>操作记录</h2>
        ${
          state.logs.length
            ? state.logs
                .slice(0, 30)
                .map(
                  (entry) => `
                    <div class="log-line">
                      <span>${escapeHtml(entry.level)}</span>
                      <time>${new Date(entry.timestamp).toLocaleString(state.appPreferences?.language ?? "zh-CN")}</time>
                      <p>${escapeHtml(entry.event)}<br><code>${escapeHtml(entry.path)}</code></p>
                    </div>`,
                )
                .join("")
            : `<div class="log-line"><span>INFO</span><time>—</time><p>尚未记录持久化操作</p></div>`
        }
      </aside>
    </div>
  `;
}

function renderDiagnostic(issue: DiagnosticIssue): string {
  const repairable = issue.repairable && isDirectDiagnosticRepair(issue);
  const toolId = diagnosticToolTarget(issue);
  const provider = activeAiProvider();
  const aiReady = Boolean(provider?.selectedModel && provider.apiKeyConfigured);
  return `
    <article class="diagnostic-item ${issue.level}">
      <span>${icon(issue.level === "error" ? "CircleX" : issue.level === "warning" ? "TriangleAlert" : "Info", 19)}</span>
      <div><strong>${escapeHtml(issue.title)}</strong><p>${escapeHtml(issue.detail)}</p>${issue.evidence ? `<code>${escapeHtml(issue.evidence)}</code>` : ""}</div>
      <div class="diagnostic-actions">
        <button class="analysis-action" data-local-guidance="${encodeURIComponent(issue.code)}">${icon("ClipboardCheck", 14)} 本地分析与建议</button>
        ${
          repairable
            ? `<button class="repair-action" data-repair-issue="${encodeURIComponent(issue.code)}">${icon("Wrench", 14)} 生成修复计划</button>`
            : toolId
              ? `<button class="repair-action" data-open-issue-tool="${toolId}">${icon("PanelTopOpen", 14)} 打开工具详情</button>`
              : `<span class="manual-action">${icon("ShieldAlert", 14)} ${issue.code.includes("_系统") ? "系统级，仅提供处理方案" : "需要人工判断"}</span>`
        }
        <button class="ai-action" data-ai-issue="${encodeURIComponent(issue.code)}" ${state.aiAnalyzing ? "disabled" : ""}>${icon(state.aiAnalyzing ? "LoaderCircle" : "Sparkles", 14)} ${state.aiAnalyzing ? "分析中" : aiReady ? "AI 增强分析" : "配置 AI"}</button>
      </div>
    </article>
  `;
}

function isDirectDiagnosticRepair(issue: DiagnosticIssue): boolean {
  return (
    issue.code.startsWith("PATH_DUPLICATE_用户") ||
    issue.code.startsWith("PATH_MISSING_用户") ||
    issue.code.startsWith("PATH_EMPTY_用户") ||
    issue.code.startsWith("PATH_RELATIVE_用户") ||
    issue.code.startsWith("ENV_DUPLICATE_SCOPE_")
  );
}

function diagnosticToolTarget(issue: DiagnosticIssue): string | undefined {
  if (issue.code === "JAVA_HOME_DEFAULT_MISMATCH") return "java";
  if (issue.code === "ANDROID_ROOT_CONFLICT") return "android-sdk";
  const candidates = [
    "android-sdk",
    "android-ndk",
    "python",
    "gradle",
    "node",
    "maven",
    "dotnet",
    "ruby",
    "php",
    "cmake",
    "java",
    "rust",
    "git",
    "adb",
    "go",
  ];
  return candidates.find((toolId) =>
    issue.code.startsWith(toolId.replaceAll("-", "_").toUpperCase()),
  );
}

function activeAiProvider(): AiProviderConfig | undefined {
  const activeId = state.aiSettings?.activeProviderId;
  return state.aiSettings?.providers.find((provider) => provider.id === activeId);
}

function toolCommandPrefix(toolId: string): string {
  return toolId === "java" ? "jdk" : toolId;
}

function renderCommands(): string {
  const status = state.terminalCommands;
  const ready =
    status && status.scriptCount === status.expectedScriptCount;
  return `
    <div class="page-heading">
      <div><p class="eyebrow">TERMINAL COMMANDS</p><h1>工具命令说明</h1><p>安装一次命令脚本后，可在任意新开的 CMD 或 PowerShell 中直接管理各工具。</p></div>
      <div class="command-status ${status?.enabledInUserPath ? "enabled" : ""}">
        ${icon(status?.enabledInUserPath ? "CircleCheck" : "CircleDashed", 17)}
        <span>${status?.enabledInUserPath ? "用户 PATH 已启用" : "尚未加入用户 PATH"}</span>
      </div>
    </div>
    <section class="panel command-setup">
      <div>
        <p class="eyebrow">COMMAND DIRECTORY</p>
        <h2>命令脚本目录</h2>
        <p>脚本调用同一个 EnvNexus AI 主程序。PATH 变更会先显示差异、备份与确认计划。</p>
        <div class="command-directory-editor">
          <input
            id="terminal-command-directory"
            type="text"
            value="${escapeHtml(status?.directory ?? "")}"
            placeholder="例如 E:\\Environment\\EnvNexusAICommands"
            aria-label="命令脚本保存目录"
            ${status?.enabledInUserPath ? "disabled" : ""}
          />
          <button class="secondary-button" id="select-terminal-command-directory" ${status?.enabledInUserPath ? "disabled" : ""}>${icon("FolderOpen", 15)} 浏览</button>
          <button class="secondary-button" id="save-terminal-command-directory" ${status?.enabledInUserPath ? "disabled" : ""}>${icon("Save", 15)} 保存目录</button>
        </div>
        <small>${status ? `${status.scriptCount}/${status.expectedScriptCount} 个脚本已生成` : "正在检查命令状态"}${ready ? " · 脚本完整" : ""}</small>
        ${
          status?.enabledInUserPath
            ? `<small class="command-directory-lock">${icon("LockKeyhole", 12)} 如需更换目录，请先从用户 PATH 停用当前目录，避免新旧命令同时生效。</small>`
            : ""
        }
      </div>
      <div class="command-setup-actions">
        ${
          status?.enabledInUserPath
            ? `<button class="primary-button" id="repair-terminal-commands">${icon("Wrench", 16)} 重新生成 / 修复脚本</button>
               <button class="secondary-button danger" id="disable-terminal-commands">${icon("Power", 16)} 从用户 PATH 停用</button>`
            : `<button class="primary-button" id="enable-terminal-commands">${icon("TerminalSquare", 16)} 生成并启用命令</button>`
        }
        <span>启用后请关闭并重新打开终端。</span>
      </div>
    </section>
    <section class="panel command-global-help">
      <div class="panel-title"><div><p class="eyebrow">GLOBAL</p><h2>全局命令</h2></div></div>
      <div class="command-chip-row">
        <code>env-tools</code><span>列出全部支持工具和当前保存的安装目录</span>
        <code>env-scan</code><span>手动扫描 PATH、版本管理器和已保存的工具目录，并保存快照</span>
        <code>env-diagnose</code><span>读取上次快照中的诊断</span>
        <code>env-repair &lt;code&gt;</code><span>预览诊断修复；追加 <b>--yes</b> 后确认执行</span>
      </div>
      <p class="command-cache-note">${icon("Info", 14)} 命令脚本不保存工具目录。之后修改目录时，<code>*-root</code> 和 <code>*-install</code> 会读取最新设置；<code>*-list</code> 读取上次扫描快照，如目录内容有变化请手动执行 <code>env-scan</code>。</p>
    </section>
    <div class="command-tool-grid">
      ${state.toolDefinitions
        .map((tool) => {
          const prefix = toolCommandPrefix(tool.id);
          return `
            <article class="panel command-tool-card" data-command-tool="${tool.id}">
              <header><span class="tool-mark tool-${escapeHtml(tool.id)}">${renderToolBrand(tool.id, tool.icon, 24)}</span><div><strong>${escapeHtml(tool.displayName)}</strong><small>${escapeHtml(tool.category)}</small></div></header>
              <div class="command-list">
                <p><code>${prefix}-list</code><span>已安装版本与当前默认版本</span></p>
                <p><code>${prefix}-versions</code><span>查询官方可安装版本</span></p>
                <p><code>${prefix}-root get</code><span>查看管理目录</span></p>
                <p><code>${prefix}-root set "E:\\..."</code><span>设置管理目录</span></p>
                <p><code>${prefix}-install &lt;version&gt;</code><span>预览安装；加 <b>--yes</b> 执行</span></p>
                <p><code>${prefix}-use &lt;path&gt;</code><span>预览默认版本切换</span></p>
                <p><code>${prefix}-repair &lt;path&gt;</code><span>预览受管版本修复</span></p>
                <p><code>${prefix}-uninstall &lt;path&gt;</code><span>预览受管版本卸载</span></p>
              </div>
            </article>`;
        })
        .join("")}
    </div>
    <section class="panel command-safety-note">
      ${icon("ShieldCheck", 17)}
      <p><strong>执行规则：</strong>list/versions/diagnose 不修改环境；install/use/repair/uninstall 默认只显示计划，必须追加 <code>--yes</code> 才执行。系统级 PATH 与 HKLM 始终不在命令脚本的写入范围。</p>
    </section>
  `;
}

function renderSettings(): string {
  const selected = getStoredTheme();
  const preferences = state.appPreferences ?? {
    schemaVersion: 1,
    closeBehavior: "exit",
    startMinimized: false,
    launchAtLogin: false,
    language: "zh-CN" as AppLanguage,
  };
  return `
    <div class="page-heading compact"><div><p class="eyebrow">PREFERENCES</p><h1>设置</h1><p>应用行为、外观与安全边界均保存在 App 数据目录。</p></div></div>
    <section class="settings-section panel app-behavior-section">
      <div class="app-behavior-heading">
        <div class="settings-copy">
          <p class="eyebrow">APP BEHAVIOR</p>
          <h2>应用与启动</h2>
          <p>集中设置关闭、启动、语言和托盘行为。</p>
        </div>
        <div class="tray-capabilities">
          ${icon("PanelTopOpen", 18)}
          <div><strong>Windows 托盘 · ${state.trayReady ? "已就绪" : "正在初始化"}</strong><span>双击打开，右键管理工具链、版本和诊断</span></div>
        </div>
      </div>
      <div class="app-behavior-controls">
        <label class="behavior-field">
          <span>点击窗口关闭按钮时</span>
          <select id="app-close-behavior">
            <option value="minimizeToTray" ${preferences.closeBehavior === "minimizeToTray" ? "selected" : ""}>最小化到系统托盘，继续后台运行</option>
            <option value="exit" ${preferences.closeBehavior === "exit" ? "selected" : ""}>直接退出程序</option>
          </select>
        </label>
        <label class="behavior-field">
          <span>界面与托盘语言</span>
          <select id="app-language">
            <option value="zh-CN" ${preferences.language === "zh-CN" ? "selected" : ""}>简体中文</option>
            <option value="zh-TW" ${preferences.language === "zh-TW" ? "selected" : ""}>繁體中文</option>
            <option value="en-US" ${preferences.language === "en-US" ? "selected" : ""}>English</option>
            <option value="ja-JP" ${preferences.language === "ja-JP" ? "selected" : ""}>日本語</option>
            <option value="ko-KR" ${preferences.language === "ko-KR" ? "selected" : ""}>한국어</option>
          </select>
        </label>
        <label class="behavior-toggle">
          <input id="app-launch-at-login" type="checkbox" ${preferences.launchAtLogin ? "checked" : ""}>
          <span><strong>登录 Windows 后自动启动</strong><small>写入当前用户的启动项，不需要管理员权限。</small></span>
        </label>
        <label class="behavior-toggle">
          <input id="app-start-minimized" type="checkbox" ${preferences.startMinimized ? "checked" : ""}>
          <span><strong>启动后隐藏到托盘</strong><small>下次启动生效，不会触发扫描或 AI 请求。</small></span>
        </label>
      </div>
      <div class="app-behavior-actions">
        <button class="secondary-button" id="hide-to-tray">${icon("Minimize2", 16)} 隐藏到托盘</button>
        <button class="primary-button" id="save-app-preferences">${icon("Save", 16)} 保存设置</button>
      </div>
    </section>
    <section class="settings-section panel">
      <div class="settings-copy"><p class="eyebrow">APPEARANCE</p><h2>界面主题</h2><p>切换仅影响显示，不影响工具或环境配置。</p></div>
      <div class="theme-grid">
        ${themes
          .map(
            (theme) => `
              <button class="theme-choice ${selected === theme.id ? "selected" : ""}" data-theme="${theme.id}">
                <span class="theme-preview theme-preview-${theme.id}"><i></i><i></i><i></i></span>
                <strong>${theme.label}</strong><small>${theme.hint}</small>
                ${selected === theme.id ? icon("CircleCheck", 17) : ""}
              </button>
            `,
          )
          .join("")}
      </div>
    </section>
    <section class="settings-section panel horizontal">
      <div class="settings-copy"><p class="eyebrow">STORAGE</p><h2>数据根目录</h2><p>配置、缓存、日志、备份和下载包统一存放。</p></div>
      <div class="path-setting"><code>${escapeHtml(state.bootstrap?.dataRoot ?? "正在读取…")}</code><button class="secondary-button" id="change-data-root">更改</button></div>
    </section>
    ${renderApplicationUpdate()}
    ${renderAiSettings()}
  `;
}

function renderApplicationUpdate(): string {
  const status = state.applicationUpdate;
  const currentVersion = state.bootstrap?.appVersion ?? "0.1.0";
  const statusIcon =
    status?.phase === "error"
      ? "CircleAlert"
      : status?.phase === "current"
        ? "BadgeCheck"
        : status?.phase === "available"
          ? "PackageOpen"
          : status?.phase === "downloading"
            ? "Download"
            : "RefreshCw";
  const notes = status?.notes
    ? `<div class="update-notes"><strong>发布说明</strong><p>${escapeHtml(status.notes).replaceAll("\n", "<br>")}</p></div>`
    : "";
  const progress =
    status?.phase === "downloading"
      ? `<div class="update-progress"><div><i style="width:${status.progressPercent ?? 3}%"></i></div><span>${Math.round(status.progressPercent ?? 0)}%</span></div>`
      : "";
  return `
    <section class="settings-section panel update-settings-section">
      <div class="settings-copy">
        <p class="eyebrow">APPLICATION UPDATE</p>
        <h2>应用更新</h2>
        <p>仅在点击检查时连接 GitHub。发现新版本后先显示版本与发布说明，确认后才下载经过签名验证的安装包。</p>
      </div>
      <div class="update-settings-content">
        <div class="update-version-row">
          <span class="update-brand-icon">${icon("GitFork", 20)}</span>
          <div><small>当前版本</small><strong>EnvNexus AI ${escapeHtml(currentVersion)}</strong><code>PuppetWen/EnvNexus-AI</code></div>
          <button class="secondary-button" id="check-app-update" ${status?.phase === "checking" || status?.phase === "downloading" ? "disabled" : ""}>${icon("RefreshCw", 16)} ${status?.phase === "checking" ? "正在检查…" : "检查更新"}</button>
        </div>
        ${
          status
            ? `<div class="update-status update-status-${status.phase}">
                ${icon(statusIcon as keyof typeof icons, 18)}
                <div><strong>${status.phase === "available" ? `发现 EnvNexus AI ${escapeHtml(status.availableVersion ?? "")}` : escapeHtml(status.message)}</strong>
                ${status.phase === "available" ? `<span>${escapeHtml(status.message)}</span>` : ""}</div>
                ${status.phase === "available" ? `<button class="primary-button" id="install-app-update">${icon("Download", 16)} 确认并更新</button>` : ""}
              </div>${notes}${progress}`
            : `<div class="update-idle-note">${icon("ShieldCheck", 16)} 更新元数据来自 GitHub Releases，安装前由内置公钥验证签名；不会在启动时自动联网。</div>`
        }
        <p class="update-portable-note">${icon("Info", 14)} Windows 安装版会原地升级；便携版确认更新后会启动当前用户安装程序，原便携文件不会被静默覆盖。</p>
      </div>
    </section>
  `;
}

function renderAiSettings(): string {
  const providers = state.aiSettings?.providers ?? [];
  const providerId =
    state.selectedAiProviderId ??
    state.aiSettings?.activeProviderId ??
    providers[0]?.id;
  const provider = providers.find((candidate) => candidate.id === providerId);
  if (!provider) {
    return `
      <section class="settings-section panel">
        <div class="settings-copy"><p class="eyebrow">AI ASSIST</p><h2>AI 分析与修复建议</h2><p>正在读取 AI 厂商配置…</p></div>
      </section>`;
  }
  const modelOptions = [...state.aiModels];
  const providerIsActive = state.aiSettings?.activeProviderId === provider.id;
  const providerIsReady = provider.apiKeyConfigured && Boolean(provider.selectedModel);
  if (
    provider.selectedModel &&
    !modelOptions.some((model) => model.id === provider.selectedModel)
  ) {
    modelOptions.unshift({
      id: provider.selectedModel,
      displayName: `${provider.selectedModel}（已保存）`,
    });
  }
  return `
    <section class="settings-section panel ai-settings-section">
      <div class="settings-copy">
        <p class="eyebrow">AI ASSIST</p>
        <h2>AI 分析与修复建议</h2>
        <p>支持 OpenAI、Claude、Kimi、DeepSeek、GLM、Grok、Qwen、Gemini 和第三方兼容服务。AI 只分析用户主动发送的单条诊断，不直接修改环境。</p>
      </div>
      <div class="ai-provider-tabs">
        ${providers
          .map(
            (candidate) => {
              const isCurrent = state.aiSettings?.activeProviderId === candidate.id;
              const isReady =
                candidate.apiKeyConfigured && Boolean(candidate.selectedModel);
              const status = isCurrent
                ? `当前使用 · ${candidate.selectedModel}`
                : isReady
                  ? `可切换 · ${candidate.selectedModel}`
                  : candidate.apiKeyConfigured
                    ? "密钥已保存 · 未选择模型"
                    : "未配置";
              return `
              <button class="${candidate.id === provider.id ? "active" : ""} ${isCurrent ? "current" : ""}" data-ai-provider="${candidate.id}">
                <span class="ai-provider-brand">${aiProviderIcon(candidate.id, 22)}</span>
                <span class="ai-provider-copy">
                  <strong>${escapeHtml(candidate.displayName)}</strong>
                  <small class="${candidate.apiKeyConfigured ? "configured" : ""}">${escapeHtml(status)}</small>
                </span>
              </button>`;
            },
          )
          .join("")}
      </div>
      <div class="ai-config-form" data-ai-provider-form="${provider.id}">
        <div class="ai-config-provider wide">
          <span class="ai-config-provider-icon">${aiProviderIcon(provider.id, 30)}</span>
          <span><strong>${escapeHtml(provider.displayName)}</strong><small>此处只编辑并保存该厂商自己的 URL、协议、密钥和模型。</small></span>
          <em class="${providerIsActive ? "active" : providerIsReady ? "ready" : ""}">${providerIsActive ? "当前使用中" : providerIsReady ? "配置有效" : "配置未完成"}</em>
        </div>
        <label>
          <span>显示名称</span>
          <input id="ai-display-name" value="${escapeHtml(provider.displayName)}" autocomplete="off">
        </label>
        <label>
          <span>API 协议</span>
          <select id="ai-protocol">
            <option value="openai" ${provider.protocol === "openai" ? "selected" : ""}>OpenAI Compatible</option>
            <option value="anthropic" ${provider.protocol === "anthropic" ? "selected" : ""}>Anthropic Messages</option>
            <option value="gemini" ${provider.protocol === "gemini" ? "selected" : ""}>Google Gemini</option>
          </select>
        </label>
        <label class="wide">
          <span>API 基础 URL</span>
          <input id="ai-base-url" value="${escapeHtml(provider.baseUrl)}" spellcheck="false" autocomplete="off">
        </label>
        <label class="wide">
          <span>API Key</span>
          <input id="ai-api-key" type="password" value="" placeholder="${provider.apiKeyConfigured ? "已使用 Windows DPAPI 加密保存；留空表示不更换" : "输入 API Key"}" autocomplete="new-password">
        </label>
        <div class="ai-config-actions wide">
          <div>
            <button class="primary-button" id="save-ai-provider">${icon("Save", 16)} 保存连接</button>
            <button class="secondary-button" id="fetch-ai-models" ${provider.apiKeyConfigured ? "" : ""}>${icon("CloudDownload", 16)} 保存并远程获取模型</button>
            <button class="secondary-button ai-activate-button" id="activate-ai-provider" ${providerIsReady && !providerIsActive ? "" : "disabled"}>${icon(providerIsActive ? "CircleCheck" : "Power", 16)} ${providerIsActive ? "当前使用中" : "设为当前 AI"}</button>
            ${provider.apiKeyConfigured ? `<button class="text-button danger" id="clear-ai-key">删除密钥</button>` : ""}
          </div>
          <span>${provider.apiKeyConfigured ? `${icon("ShieldCheck", 14)} 密钥由当前 Windows 用户的 DPAPI 加密，前端不会读回明文。` : `${icon("KeyRound", 14)} 保存后才会连接厂商；EnvNexus AI 不会在启动时请求 AI。`}</span>
        </div>
        <label class="wide">
          <span>用于诊断的模型</span>
          <select id="ai-model-select" ${modelOptions.length ? "" : "disabled"}>
            ${
              modelOptions.length
                ? modelOptions
                    .map(
                      (model) =>
                        `<option value="${escapeHtml(model.id)}" ${provider.selectedModel === model.id ? "selected" : ""}>${escapeHtml(model.displayName)} · ${escapeHtml(model.id)}</option>`,
                    )
                    .join("")
                : `<option>请先保存连接并远程获取模型</option>`
            }
          </select>
        </label>
        <label class="wide">
          <span>手动模型 ID（厂商未提供模型列表接口时使用）</span>
          <div class="manual-model-row">
            <input id="ai-manual-model" value="${escapeHtml(provider.selectedModel ?? "")}" placeholder="例如 qwen-plus、glm-4.6、第三方部署 ID" spellcheck="false" autocomplete="off">
            <button class="secondary-button" id="save-ai-manual-model">保存模型 ID</button>
          </div>
        </label>
      </div>
      <div class="ai-privacy-note">
        ${icon("ShieldAlert", 16)}
        <p><strong>发送边界</strong>：只有点击某条诊断的“AI 分析”并再次确认后，标题、描述、证据路径和已识别版本管理器才会发送到所选服务。API Key 不会包含在提示词中。</p>
      </div>
    </section>
  `;
}

function collectAiProviderInput(root: HTMLElement): AiProviderInput | undefined {
  const form = root.querySelector<HTMLElement>("[data-ai-provider-form]");
  const id = form?.dataset.aiProviderForm;
  const displayName = root.querySelector<HTMLInputElement>("#ai-display-name")?.value;
  const protocol = root.querySelector<HTMLSelectElement>("#ai-protocol")?.value;
  const baseUrl = root.querySelector<HTMLInputElement>("#ai-base-url")?.value;
  const apiKey = root.querySelector<HTMLInputElement>("#ai-api-key")?.value.trim();
  const selectedModel =
    state.aiSettings?.providers.find((provider) => provider.id === id)?.selectedModel;
  if (
    !id ||
    displayName === undefined ||
    baseUrl === undefined ||
    !["openai", "anthropic", "gemini"].includes(protocol ?? "")
  ) {
    return undefined;
  }
  return {
    id,
    displayName,
    protocol: protocol as AiProviderInput["protocol"],
    baseUrl,
    selectedModel,
    apiKey: apiKey || undefined,
  };
}

function renderOverlays(): string {
  if (state.pendingPlan) return renderPlanModal(state.pendingPlan);
  if (state.diagnosticGuidance) {
    return renderDiagnosticGuidanceModal(state.diagnosticGuidance);
  }
  if (state.aiAnalysis) return renderAiAnalysisModal(state.aiAnalysis);
  return "";
}

function renderDiagnosticGuidanceModal(guidance: DiagnosticGuidance): string {
  const aiReady = Boolean(
    activeAiProvider()?.apiKeyConfigured && activeAiProvider()?.selectedModel,
  );
  const list = (items: string[]) =>
    items.length
      ? `<ul>${items.map((item) => `<li>${escapeHtml(item)}</li>`).join("")}</ul>`
      : `<p class="guidance-empty">没有额外项目。</p>`;
  return `
    <div class="overlay plan-overlay">
      <section class="plan-modal panel diagnostic-guidance-modal" role="dialog" aria-modal="true" aria-label="本地诊断分析与修复建议">
        <header class="plan-header">
          <div><p class="eyebrow">LOCAL DIAGNOSTIC ENGINE</p><h2>${escapeHtml(guidance.summary)}</h2><p>${escapeHtml(guidance.analysisSource)} · ${escapeHtml(guidance.issueCode)}</p></div>
          <button class="icon-button" id="close-diagnostic-guidance">${icon("X")}</button>
        </header>
        <div class="diagnostic-guidance-body">
          <div class="guidance-safety">${icon("ShieldCheck", 16)} 本地规则始终可用；只有可唯一判断、仅修改用户环境且能备份回滚的项目才提供一键修复。</div>
          <div class="guidance-columns">
            <section><p class="eyebrow">ROOT CAUSES</p><h3>原因与证据</h3>${list(guidance.rootCauses)}</section>
            <section><p class="eyebrow">THIS COMPUTER</p><h3>本机适配因素</h3>${list(guidance.machineFactors)}</section>
          </div>
          <section class="guidance-recommendations"><p class="eyebrow">RECOMMENDATIONS</p><h3>修复建议</h3>${list(guidance.recommendations)}</section>
          <section class="guidance-commands">
            <p class="eyebrow">COPYABLE COMMANDS</p><h3>可复制命令</h3>
            <div>
              ${guidance.commands
                .map(
                  (command, index) => `
                    <article class="${command.changesEnvironment ? "changes-environment" : ""}">
                      <span><strong>${escapeHtml(command.label)}</strong><small>${escapeHtml(command.shell)}${command.requiresElevation ? " · 需要管理员权限" : command.changesEnvironment ? " · 会修改用户环境" : " · 只读"}</small></span>
                      <code>${escapeHtml(command.command)}</code>
                      <button class="secondary-button" data-copy-guidance-command="${index}">${icon("Copy", 14)} 复制</button>
                    </article>`,
                )
                .join("")}
            </div>
          </section>
        </div>
        <footer class="plan-footer">
          <span>${icon(guidance.requiresElevation ? "ShieldAlert" : "ShieldCheck", 14)} ${guidance.requiresElevation ? "涉及系统级配置，EnvNexus AI 不会直接执行" : "任何写入仍需差异预览与确认"}</span>
          <div>
            <button class="secondary-button" id="guidance-ai-analysis">${icon("Sparkles", 15)} ${aiReady ? "AI 增强分析" : "配置 AI"}</button>
            ${
              guidance.oneClickAvailable
                ? `<button class="primary-button" id="guidance-one-click">${icon("Wrench", 15)} ${escapeHtml(guidance.oneClickLabel ?? "预览一键修复")}</button>`
                : ""
            }
            <button class="secondary-button" id="close-diagnostic-guidance">关闭</button>
          </div>
        </footer>
      </section>
    </div>
  `;
}

function renderAiAnalysisModal(analysis: AiDiagnosticAnalysis): string {
  return `
    <div class="overlay plan-overlay">
      <section class="plan-modal panel ai-analysis-modal" role="dialog" aria-modal="true" aria-label="AI 诊断分析">
        <header class="plan-header">
          <div><p class="eyebrow">AI DIAGNOSTIC ANALYSIS</p><h2>${escapeHtml(analysis.issueCode)}</h2><p>${escapeHtml(analysis.providerName)} · ${escapeHtml(analysis.model)} · ${new Date(analysis.generatedAt).toLocaleString(state.appPreferences?.language ?? "zh-CN")}</p></div>
          <button class="icon-button" id="close-ai-analysis">${icon("X")}</button>
        </header>
        <div class="ai-analysis-body">
          <div class="ai-analysis-warning">${icon("TriangleAlert", 16)} AI 输出仅作为处理建议；可执行修复仍必须使用 EnvNexus AI 的本地差异计划。</div>
          <pre>${escapeHtml(analysis.content)}</pre>
        </div>
        <footer class="plan-footer">
          <span>${icon("ShieldCheck", 14)} AI 未执行任何环境变更</span>
          <button class="primary-button" id="close-ai-analysis">关闭</button>
        </footer>
      </section>
    </div>
  `;
}

function renderPlanModal(plan: OperationPlan): string {
  return `
    <div class="overlay plan-overlay">
      <section class="plan-modal panel" role="dialog" aria-modal="true" aria-label="变更确认">
        <header class="plan-header">
          <div><p class="eyebrow">CONFIRM OPERATION</p><h2>${escapeHtml(plan.title)}</h2><p>${escapeHtml(plan.summary)}</p></div>
          ${!state.applying ? `<button class="icon-button" id="cancel-plan">${icon("X")}</button>` : ""}
        </header>
        ${
          state.applying
            ? `<div class="operation-progress">
                 <span class="progress-icon">${icon(state.progress?.phase === "complete" ? "CircleCheck" : "LoaderCircle", 28)}</span>
                 <strong>${escapeHtml(state.progress?.message ?? "正在启动事务…")}</strong>
                 <div class="progress-track"><i style="width:${state.progress?.percent ?? 4}%"></i></div>
                 <small>${state.progress?.percent !== undefined ? `${state.progress.percent.toFixed(1)}%` : "请勿关闭 App"}</small>
               </div>`
            : `<div class="plan-body">
                 ${plan.requiresElevation ? `<div class="plan-blocker">${icon("ShieldAlert", 19)}<div><strong>当前计划被安全边界阻止</strong><p>系统 PATH 会遮蔽用户版本；本版本不擅自修改系统级配置。</p></div></div>` : ""}
                 ${
                   plan.warnings.length
                     ? `<div class="plan-warnings">${plan.warnings.map((warning) => `<p>${icon("TriangleAlert", 15)}<span>${escapeHtml(warning)}</span></p>`).join("")}</div>`
                     : ""
                 }
                 <div class="plan-columns">
                   <div><p class="eyebrow">STEPS</p><ol class="plan-steps">${plan.steps.map((step) => `<li class="${step.destructive ? "destructive" : ""}"><span>${icon(step.destructive ? "Trash2" : "Check", 14)}</span><p>${escapeHtml(step.description)}</p></li>`).join("")}</ol></div>
                   <div><p class="eyebrow">ENVIRONMENT DIFF</p>${renderEnvironmentDiffs(plan)}</div>
                 </div>
               </div>`
        }
        <footer class="plan-footer">
          <span>${icon("LockKeyhole", 14)} 计划将在 ${new Date(plan.expiresAt).toLocaleTimeString("zh-CN")} 过期</span>
          ${
            !state.applying
              ? `<div><button class="secondary-button" id="cancel-plan">取消</button><button class="primary-button" id="confirm-plan" ${plan.requiresElevation ? "disabled" : ""}>${icon("ShieldCheck", 16)} 确认并执行</button></div>`
              : ""
          }
        </footer>
      </section>
    </div>
  `;
}

function renderEnvironmentDiffs(plan: OperationPlan): string {
  if (plan.environmentDiffs.length === 0) {
    return `<div class="no-diff">${icon("Minus", 15)} 不修改环境变量</div>`;
  }
  return `<div class="diff-list">${plan.environmentDiffs
    .map(
      (diff) => `
        <article><strong>${escapeHtml(diff.variable)} <small>${diff.scope === "user" ? "用户级" : "系统级"}</small></strong>
          ${diff.added.map((value) => `<code class="add">+ ${escapeHtml(value)}</code>`).join("")}
          ${diff.removed.map((value) => `<code class="remove">- ${escapeHtml(value)}</code>`).join("")}
        </article>`,
    )
    .join("")}</div>`;
}

function bindEvents(root: HTMLElement): void {
  const content = root.querySelector<HTMLElement>(".content");
  content?.addEventListener(
    "wheel",
    (event) => {
      if (event.deltaY === 0) return;
      const nested = (event.target as Element | null)?.closest<HTMLElement>(
        ".backup-list, .remote-stack, .diff-list, .plan-body, .ai-analysis-body",
      );
      const scrollTarget =
        nested && nested.scrollHeight > nested.clientHeight ? nested : content;
      if (scrollTarget.scrollHeight <= scrollTarget.clientHeight) return;
      event.preventDefault();
      scrollTarget.scrollTop += event.deltaY;
    },
    { passive: false },
  );
  root.querySelectorAll<HTMLElement>("[data-nav]").forEach((element) => {
    element.addEventListener("click", () => {
      state.view = element.dataset.nav as ViewId;
      if (state.view !== "tool-detail") state.selectedToolId = undefined;
      render();
      if (state.view === "changes") void loadBackups();
      if (state.view === "diagnostics") void loadLogs();
      if (state.view === "commands") void loadTerminalCommands();
    });
  });
  root.querySelectorAll<HTMLElement>("[data-theme]").forEach((element) => {
    element.addEventListener("click", () => {
      applyTheme(element.dataset.theme as Parameters<typeof applyTheme>[0]);
      render();
    });
  });
  root.querySelector("#save-app-preferences")?.addEventListener("click", async () => {
    const closeBehavior =
      root.querySelector<HTMLSelectElement>("#app-close-behavior")?.value ===
      "minimizeToTray"
        ? "minimizeToTray"
        : "exit";
    const preferences: AppPreferences = {
      schemaVersion: 1,
      closeBehavior,
      startMinimized:
        root.querySelector<HTMLInputElement>("#app-start-minimized")?.checked ?? false,
      launchAtLogin:
        root.querySelector<HTMLInputElement>("#app-launch-at-login")?.checked ?? false,
      language:
        (root.querySelector<HTMLSelectElement>("#app-language")?.value as AppLanguage) ??
        "zh-CN",
    };
    try {
      state.appPreferences = await backend.saveAppPreferences(preferences);
      state.notice = "应用与启动设置已保存，界面和托盘菜单已刷新。";
      state.error = undefined;
    } catch (error) {
      state.error = `保存应用行为设置失败：${String(error)}`;
    }
    render();
  });
  root.querySelector("#hide-to-tray")?.addEventListener("click", async () => {
    try {
      state.notice = "EnvNexus AI 正在后台运行；双击或右键托盘图标可以恢复。";
      render();
      await backend.hideToTray();
    } catch (error) {
      state.error = `最小化到托盘失败：${String(error)}`;
      render();
    }
  });
  root
    .querySelector("#check-app-update")
    ?.addEventListener("click", () => void checkForApplicationUpdate());
  root
    .querySelector("#install-app-update")
    ?.addEventListener("click", () => void installApplicationUpdate());
  root.querySelector("#save-terminal-command-directory")?.addEventListener("click", async () => {
    const directory = root
      .querySelector<HTMLInputElement>("#terminal-command-directory")
      ?.value.trim();
    if (!directory) {
      state.error = "请输入命令脚本保存目录。";
      render();
      return;
    }
    try {
      state.terminalCommands = await backend.saveTerminalCommandDirectory(directory);
      state.notice = `命令脚本目录已保存为 ${state.terminalCommands.directory}。`;
      state.error = undefined;
    } catch (error) {
      state.error = `保存命令脚本目录失败：${String(error)}`;
    }
    render();
  });
  root.querySelector("#select-terminal-command-directory")?.addEventListener("click", async () => {
    const input = root.querySelector<HTMLInputElement>("#terminal-command-directory");
    const selected = await open({
      directory: true,
      multiple: false,
      defaultPath: input?.value || state.terminalCommands?.directory,
      title: "选择 EnvNexus AI 命令脚本保存目录",
    });
    if (typeof selected === "string" && input) input.value = selected;
  });
  root.querySelector("#terminal-command-directory")?.addEventListener("keydown", (event) => {
    if ((event as KeyboardEvent).key !== "Enter") return;
    event.preventDefault();
    root.querySelector<HTMLElement>("#save-terminal-command-directory")?.click();
  });
  root.querySelector("#enable-terminal-commands")?.addEventListener("click", async () => {
    try {
      state.pendingPlan = await backend.previewEnableTerminalCommands();
      state.error = undefined;
    } catch (error) {
      state.error = `无法生成命令启用计划：${String(error)}`;
    }
    await loadTerminalCommands(false);
    render();
  });
  root.querySelector("#repair-terminal-commands")?.addEventListener("click", async () => {
    try {
      state.terminalCommands = await backend.prepareTerminalCommands();
      state.notice = `已重新生成 ${state.terminalCommands.scriptCount} 个工具命令脚本。`;
      state.error = undefined;
    } catch (error) {
      state.error = `修复命令脚本失败：${String(error)}`;
    }
    render();
  });
  root.querySelector("#disable-terminal-commands")?.addEventListener("click", async () => {
    try {
      state.pendingPlan = await backend.previewDisableTerminalCommands();
      state.error = undefined;
    } catch (error) {
      state.error = `无法生成命令停用计划：${String(error)}`;
    }
    render();
  });
  root.querySelectorAll<HTMLButtonElement>("[data-ai-provider]").forEach((element) => {
    element.addEventListener("click", () => {
      state.selectedAiProviderId = element.dataset.aiProvider;
      state.aiModels = [];
      render();
    });
  });
  root.querySelector("#save-ai-provider")?.addEventListener("click", async () => {
    const input = collectAiProviderInput(root);
    if (!input) return;
    try {
      state.aiSettings = await backend.saveAiProvider(input);
      state.selectedAiProviderId = input.id;
      state.notice = `${input.displayName} 的独立 AI 连接设置已保存；其他厂商配置没有改变。`;
      state.error = undefined;
    } catch (error) {
      state.error = `保存 AI 设置失败：${String(error)}`;
    }
    render();
  });
  root.querySelector("#fetch-ai-models")?.addEventListener("click", async () => {
    const input = collectAiProviderInput(root);
    if (!input) return;
    try {
      state.aiSettings = await backend.saveAiProvider(input);
      state.selectedAiProviderId = input.id;
      state.aiModels = await backend.fetchAiModels(input.id);
      state.notice = `已从 ${input.displayName} 获取 ${state.aiModels.length} 个模型，请在下拉框选择。`;
      state.error = undefined;
    } catch (error) {
      state.error = `获取 AI 模型失败：${String(error)}。如果该厂商未提供模型列表接口，请在下方手动填写官方模型 ID。`;
    }
    render();
  });
  root.querySelector("#ai-model-select")?.addEventListener("change", async (event) => {
    const providerId = state.selectedAiProviderId ?? state.aiSettings?.activeProviderId;
    const model = (event.currentTarget as HTMLSelectElement).value;
    if (!providerId || !model) return;
    try {
      state.aiSettings = await backend.selectAiModel(providerId, model);
      state.notice = `${providerId} 的模型已保存为 ${model}；当前使用厂商不会自动改变。`;
      state.error = undefined;
    } catch (error) {
      state.error = `保存 AI 模型失败：${String(error)}`;
    }
    render();
  });
  root.querySelector("#save-ai-manual-model")?.addEventListener("click", async () => {
    const providerId = state.selectedAiProviderId ?? state.aiSettings?.activeProviderId;
    const model = root.querySelector<HTMLInputElement>("#ai-manual-model")?.value.trim();
    if (!providerId || !model) {
      state.error = "请输入模型 ID。";
      render();
      return;
    }
    try {
      state.aiSettings = await backend.selectAiModel(providerId, model);
      state.notice = `${providerId} 的模型已保存为 ${model}；当前使用厂商不会自动改变。`;
      state.error = undefined;
    } catch (error) {
      state.error = `保存 AI 模型失败：${String(error)}`;
    }
    render();
  });
  root.querySelector("#activate-ai-provider")?.addEventListener("click", async () => {
    const providerId = state.selectedAiProviderId ?? state.aiSettings?.activeProviderId;
    if (!providerId) return;
    try {
      state.aiSettings = await backend.activateAiProvider(providerId);
      const provider = state.aiSettings.providers.find(
        (candidate) => candidate.id === providerId,
      );
      state.notice = `当前诊断 AI 已切换为 ${provider?.displayName ?? providerId} / ${provider?.selectedModel ?? ""}。`;
      state.error = undefined;
    } catch (error) {
      state.error = `切换 AI 厂商失败：${String(error)}`;
    }
    render();
  });
  root.querySelector("#clear-ai-key")?.addEventListener("click", async () => {
    const providerId = state.selectedAiProviderId ?? state.aiSettings?.activeProviderId;
    if (
      !providerId ||
      !window.confirm("删除该厂商由 Windows DPAPI 加密保存的 API Key？")
    ) {
      return;
    }
    try {
      state.aiSettings = await backend.clearAiApiKey(providerId);
      state.aiModels = [];
      state.notice = "AI API Key 已删除。";
      state.error = undefined;
    } catch (error) {
      state.error = `删除 AI API Key 失败：${String(error)}`;
    }
    render();
  });
  root.querySelectorAll<HTMLElement>("[data-open-tool]").forEach((element) => {
    element.addEventListener("click", (event) => {
      event.stopPropagation();
      state.selectedToolId = element.dataset.openTool;
      state.view = "tool-detail";
      render();
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-local-guidance]").forEach((element) => {
    element.addEventListener("click", async () => {
      const issueCode = element.dataset.localGuidance;
      if (!issueCode) return;
      try {
        state.diagnosticGuidance = await backend.diagnosticGuidance(
          decodeURIComponent(issueCode),
        );
        state.error = undefined;
      } catch (error) {
        state.error = `生成本地诊断建议失败：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-repair-issue]").forEach((element) => {
    element.addEventListener("click", async () => {
      const issueCode = element.dataset.repairIssue;
      if (!issueCode) return;
      try {
        state.pendingPlan = await backend.previewDiagnosticRepair(
          decodeURIComponent(issueCode),
        );
        state.error = undefined;
      } catch (error) {
        state.error = `无法生成诊断修复计划：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll("#close-diagnostic-guidance").forEach((element) => {
    element.addEventListener("click", () => {
      state.diagnosticGuidance = undefined;
      render();
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-copy-guidance-command]").forEach((element) => {
    element.addEventListener("click", async () => {
      const index = Number(element.dataset.copyGuidanceCommand);
      const command = state.diagnosticGuidance?.commands[index]?.command;
      if (!command) return;
      try {
        await navigator.clipboard.writeText(command);
        state.notice = "命令已复制到剪贴板。";
        render();
      } catch {
        window.prompt("请复制下面的命令：", command);
      }
    });
  });
  root.querySelector("#guidance-one-click")?.addEventListener("click", async () => {
    const issueCode = state.diagnosticGuidance?.issueCode;
    if (!issueCode) return;
    try {
      state.pendingPlan = await backend.previewDiagnosticRepair(issueCode);
      state.diagnosticGuidance = undefined;
      state.error = undefined;
    } catch (error) {
      state.error = `无法生成一键修复计划：${String(error)}`;
    }
    render();
  });
  root.querySelector("#guidance-ai-analysis")?.addEventListener("click", async () => {
    const issueCode = state.diagnosticGuidance?.issueCode;
    const activeAi = activeAiProvider();
    if (!issueCode) return;
    if (!activeAi?.apiKeyConfigured || !activeAi.selectedModel) {
      state.diagnosticGuidance = undefined;
      state.view = "settings";
      state.selectedAiProviderId =
        state.aiSettings?.activeProviderId ?? state.aiSettings?.providers[0]?.id;
      state.notice = "请先保存 AI URL 与 API Key，远程获取并选择模型。";
      render();
      return;
    }
    const confirmed = window.confirm(
      `将诊断证据、本机架构、相关工具版本、保存目录和版本管理器信息发送到 ${activeAi.displayName} / ${activeAi.selectedModel}。\n\nAPI Key 不会包含在提示词中。是否继续？`,
    );
    if (!confirmed) return;
    state.diagnosticGuidance = undefined;
    state.aiAnalyzing = true;
    state.error = undefined;
    render();
    try {
      state.aiAnalysis = await backend.analyzeDiagnosticWithAi(issueCode);
    } catch (error) {
      state.error = `AI 诊断失败：${String(error)}`;
    } finally {
      state.aiAnalyzing = false;
      render();
    }
  });
  root.querySelectorAll<HTMLButtonElement>("[data-open-issue-tool]").forEach((element) => {
    element.addEventListener("click", () => {
      const toolId = element.dataset.openIssueTool;
      if (!toolId) return;
      state.selectedToolId = toolId;
      state.view = "tool-detail";
      render();
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-ai-issue]").forEach((element) => {
    element.addEventListener("click", async () => {
      const activeAi = activeAiProvider();
      if (!activeAi?.apiKeyConfigured || !activeAi.selectedModel) {
        state.view = "settings";
        state.selectedAiProviderId =
          state.aiSettings?.activeProviderId ?? state.aiSettings?.providers[0]?.id;
        state.notice = "请先保存 AI URL 与 API Key，远程获取并选择模型。";
        render();
        return;
      }
      const issueCode = element.dataset.aiIssue;
      if (!issueCode) return;
      const issue = [
        ...(state.scan?.issues ?? []),
        ...(state.scan?.tools.flatMap((tool) => tool.issues) ?? []),
      ].find((candidate) => candidate.code === decodeURIComponent(issueCode));
      if (!issue) return;
      const confirmed = window.confirm(
        `将以下内容发送到 ${activeAi.displayName} / ${activeAi.selectedModel}：\n\n` +
          `诊断标题、描述、证据路径，以及已识别的版本管理器信息。\n\n` +
          `API Key 不会包含在提示词中。是否继续？`,
      );
      if (!confirmed) return;
      state.aiAnalyzing = true;
      state.error = undefined;
      render();
      try {
        state.aiAnalysis = await backend.analyzeDiagnosticWithAi(issue.code);
      } catch (error) {
        state.error = `AI 诊断失败：${String(error)}`;
      } finally {
        state.aiAnalyzing = false;
        render();
      }
    });
  });
  root.querySelectorAll("#close-ai-analysis").forEach((element) => {
    element.addEventListener("click", () => {
      state.aiAnalysis = undefined;
      render();
    });
  });
  const searchInput = root.querySelector<HTMLInputElement>("#tool-search");
  const filterButtons = Array.from(
    root.querySelectorAll<HTMLButtonElement>("[data-tool-filter]"),
  );
  // 搜索词和筛选放在全局 state 里，异步刷新（扫描完成、通知等）重建 DOM 后不丢失
  const applyToolFilter = (): void => {
    const query = state.toolSearchQuery.trim().toLocaleLowerCase();
    root.querySelectorAll<HTMLElement>("[data-tool-card]").forEach((card) => {
      const matchesQuery = (card.dataset.search ?? "").includes(query);
      const matchesFilter =
        state.toolFilter === "all" ||
        (state.toolFilter === "installed" && card.dataset.installed === "true") ||
        (state.toolFilter === "issues" && card.dataset.issues === "true");
      card.hidden = !matchesQuery || !matchesFilter;
    });
    root.querySelectorAll<HTMLElement>("[data-tool-group]").forEach((group) => {
      const visibleCards = Array.from(group.querySelectorAll<HTMLElement>("[data-tool-card]")).some(
        (card) => !card.hidden,
      );
      group.hidden = !visibleCards;
    });
  };
  searchInput?.addEventListener("input", () => {
    state.toolSearchQuery = searchInput.value;
    applyToolFilter();
  });
  filterButtons.forEach((button) => {
    button.addEventListener("click", () => {
      state.toolFilter = button.dataset.toolFilter ?? "all";
      filterButtons.forEach((candidate) => candidate.classList.toggle("active", candidate === button));
      applyToolFilter();
    });
  });
  if (searchInput && (state.toolSearchQuery.trim() !== "" || state.toolFilter !== "all")) {
    applyToolFilter();
  }
  root.querySelectorAll<HTMLElement>("[data-save-android-root]").forEach((element) => {
    element.addEventListener("click", async () => {
      const path = root
        .querySelector<HTMLInputElement>("[data-android-root-input]")
        ?.value.trim();
      if (!path) {
        state.error = "请输入 Android 工具链根目录。";
        render();
        return;
      }
      try {
        const preferences = await backend.setAndroidRoot(path);
        state.androidRoot = preferences.androidRoot;
        state.toolRoots = preferences.roots;
        state.notice = `Android 工具链根目录已保存为 ${path}；现有组件不会自动迁移。`;
        state.error = undefined;
      } catch (error) {
        state.error = `保存 Android 根目录失败：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll<HTMLInputElement>("[data-android-root-input]").forEach((input) => {
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        root.querySelector<HTMLElement>("[data-save-android-root]")?.click();
      }
    });
  });
  root.querySelectorAll<HTMLElement>("[data-select-android-root]").forEach((element) => {
    element.addEventListener("click", async () => {
      const selected = await open({
        directory: true,
        multiple: false,
        defaultPath: state.androidRoot,
        title: "选择统一 Android 开发环境根目录",
      });
      if (typeof selected !== "string") return;
      try {
        const preferences = await backend.setAndroidRoot(selected);
        state.androidRoot = preferences.androidRoot;
        state.toolRoots = preferences.roots;
        state.notice = `Android 工具链根目录已保存为 ${selected}；现有 SDK 不会自动迁移。`;
        state.error = undefined;
      } catch (error) {
        state.error = `保存 Android 根目录失败：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll<HTMLElement>("[data-save-tool-root]").forEach((element) => {
    element.addEventListener("click", async () => {
      const toolId = element.dataset.saveToolRoot;
      if (!toolId) return;
      const input = Array.from(
        root.querySelectorAll<HTMLInputElement>("[data-tool-root-input]"),
      ).find((candidate) => candidate.dataset.toolRootInput === toolId);
      const path = input?.value.trim();
      if (!path) {
        state.error = "请输入该工具的默认安装根目录。";
        render();
        return;
      }
      const sharedAndroidRoot = androidWorkspaceTools.has(toolId);
      try {
        const preferences = await backend.setToolRoot(toolId, path);
        state.androidRoot = preferences.androidRoot;
        state.toolRoots = preferences.roots;
        state.notice = sharedAndroidRoot
          ? `Android 工具链根目录已同步为 ${path}。`
          : `该工具的默认安装目录已保存为 ${path}。`;
        state.error = undefined;
      } catch (error) {
        state.error = `保存工具安装目录失败：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll<HTMLInputElement>("[data-tool-root-input]").forEach((input) => {
    input.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      const toolId = input.dataset.toolRootInput;
      const saveButton = Array.from(
        root.querySelectorAll<HTMLElement>("[data-save-tool-root]"),
      ).find((candidate) => candidate.dataset.saveToolRoot === toolId);
      saveButton?.click();
    });
  });
  root.querySelectorAll<HTMLElement>("[data-select-tool-root]").forEach((element) => {
    element.addEventListener("click", async () => {
      const toolId = element.dataset.selectToolRoot;
      if (!toolId) return;
      const sharedAndroidRoot = androidWorkspaceTools.has(toolId);
      const selected = await open({
        directory: true,
        multiple: false,
        defaultPath: state.toolRoots[toolId],
        title: sharedAndroidRoot
          ? "选择 Android 工具链共用根目录"
          : "选择该工具的默认安装根目录",
      });
      if (typeof selected !== "string") return;
      try {
        const preferences = await backend.setToolRoot(toolId, selected);
        state.androidRoot = preferences.androidRoot;
        state.toolRoots = preferences.roots;
        state.notice = sharedAndroidRoot
          ? `Android 工具链根目录已同步为 ${selected}。`
          : `该工具的默认安装目录已保存为 ${selected}。`;
        state.error = undefined;
      } catch (error) {
        state.error = `保存工具安装目录失败：${String(error)}`;
      }
      render();
    });
  });
  root.querySelector("#tools-rescan")?.addEventListener("click", () => void runScan());
  root.querySelector("#export-diagnostics")?.addEventListener("click", async () => {
    const stamp = new Date().toISOString().replaceAll(":", "-").slice(0, 19);
    const selected = await save({
      title: "导出 EnvNexus AI 诊断报告",
      defaultPath: `EnvNexus AI-diagnostics-${stamp}.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof selected !== "string") return;
    try {
      const path = await backend.exportDiagnosticReport(selected);
      state.notice = `诊断报告已导出到 ${path}`;
      state.error = undefined;
    } catch (error) {
      state.error = `导出诊断报告失败：${String(error)}`;
    }
    render();
  });
  root
    .querySelectorAll<HTMLElement>("#scan-button, #quick-scan, #empty-scan")
    .forEach((element) => element.addEventListener("click", () => void runScan()));
  root.querySelectorAll<HTMLButtonElement>("[data-fetch-versions]").forEach((element) => {
    element.addEventListener("click", async () => {
      const toolId = element.dataset.fetchVersions;
      if (!toolId) return;
      element.disabled = true;
      element.textContent = "查询中…";
      try {
        state.catalogs.set(toolId, await backend.fetchVersions(toolId));
        state.error = undefined;
      } catch (error) {
        state.error = `官方版本查询失败：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-install-version]").forEach((element) => {
    element.addEventListener("click", async () => {
      const toolId = state.selectedToolId;
      const version = element.dataset.installVersion
        ? decodeURIComponent(element.dataset.installVersion)
        : undefined;
      if (!toolId || !version) return;
      const selected = state.toolRoots[toolId];
      if (!selected) {
        state.error = "请先在工具详情页上方设置默认安装目录。";
        render();
        return;
      }
      try {
        state.pendingPlan = await backend.previewInstall(toolId, version, selected);
        state.error = undefined;
      } catch (error) {
        state.error = `无法生成安装计划：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-switch-path]").forEach((element) => {
    element.addEventListener("click", async () => {
      if (!state.selectedToolId || !element.dataset.switchPath) return;
      try {
        state.pendingPlan = await backend.previewSwitch(
          state.selectedToolId,
          decodeURIComponent(element.dataset.switchPath),
        );
      } catch (error) {
        state.error = `无法生成切换计划：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-uninstall-path]").forEach((element) => {
    element.addEventListener("click", async () => {
      if (!state.selectedToolId || !element.dataset.uninstallPath) return;
      try {
        state.pendingPlan = await backend.previewUninstall(
          state.selectedToolId,
          decodeURIComponent(element.dataset.uninstallPath),
        );
      } catch (error) {
        state.error = `无法生成卸载计划：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-repair-path]").forEach((element) => {
    element.addEventListener("click", async () => {
      if (!state.selectedToolId || !element.dataset.repairPath) return;
      try {
        state.pendingPlan = await backend.previewRepair(
          state.selectedToolId,
          decodeURIComponent(element.dataset.repairPath),
        );
      } catch (error) {
        state.error = `无法生成修复计划：${String(error)}`;
      }
      render();
    });
  });
  root.querySelectorAll("#cancel-plan").forEach((element) => {
    element.addEventListener("click", () => {
      state.pendingPlan = undefined;
      state.progress = undefined;
      render();
    });
  });
  root.querySelector("#confirm-plan")?.addEventListener("click", () => void applyPendingPlan());
  root.querySelector("#dismiss-notice")?.addEventListener("click", () => {
    state.notice = undefined;
    render();
  });
  root.querySelector("#refresh-backups")?.addEventListener("click", () => void loadBackups());
  root.querySelector("#change-data-root")?.addEventListener("click", async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "选择 EnvNexus AI 数据根目录",
    });
    if (typeof selected !== "string") return;
    if (
      !window.confirm(
        `下一次启动后，配置、缓存、日志、备份和下载将写入：\n${selected}\n\n现有数据不会自动迁移，是否保存？`,
      )
    ) {
      return;
    }
    try {
      const path = await backend.configureDataRoot(selected);
      state.notice = `数据目录已保存为 ${path}，重启 EnvNexus AI 后生效；现有数据未迁移。`;
      state.error = undefined;
    } catch (error) {
      state.error = `保存数据目录失败：${String(error)}`;
    }
    render();
  });
  root.querySelectorAll<HTMLButtonElement>("[data-restore-backup]").forEach((element) => {
    element.addEventListener("click", async () => {
      const backupId = element.dataset.restoreBackup;
      if (!backupId) return;
      try {
        state.pendingPlan = await backend.previewRestoreEnvironment(
          decodeURIComponent(backupId),
        );
      } catch (error) {
        state.error = `无法生成恢复计划：${String(error)}`;
      }
      render();
    });
  });
}

async function checkForApplicationUpdate(): Promise<void> {
  if (
    state.applicationUpdate?.phase === "checking" ||
    state.applicationUpdate?.phase === "downloading"
  ) {
    return;
  }
  state.applicationUpdate = {
    phase: "checking",
    message: "正在连接 GitHub Releases…",
  };
  state.error = undefined;
  render();
  try {
    if (pendingApplicationUpdate) {
      await pendingApplicationUpdate.close();
      pendingApplicationUpdate = undefined;
    }
    const update = await check({ timeout: 30_000 });
    if (!update) {
      state.applicationUpdate = {
        phase: "current",
        message: "当前已经是最新版本。",
      };
      render();
      return;
    }
    pendingApplicationUpdate = update;
    state.applicationUpdate = {
      phase: "available",
      message: `当前版本 ${update.currentVersion}，可更新到 ${update.version}。`,
      availableVersion: update.version,
      notes: update.body?.trim().slice(0, 2_000),
    };
  } catch (error) {
    pendingApplicationUpdate = undefined;
    state.applicationUpdate = {
      phase: "error",
      message: `检查更新失败：${String(error)}`,
    };
  }
  render();
}

async function installApplicationUpdate(): Promise<void> {
  const update = pendingApplicationUpdate;
  if (!update || state.applicationUpdate?.phase !== "available") return;
  const notes = update.body?.trim().slice(0, 1_200);
  const confirmed = window.confirm(
    `EnvNexus AI ${update.currentVersion} → ${update.version}\n\n` +
      `${notes ? `发布说明：\n${notes}\n\n` : ""}` +
      "更新包将从 GitHub 下载并验证签名。Windows 会在开始安装时关闭当前 App，是否继续？",
  );
  if (!confirmed) return;

  let downloaded = 0;
  let contentLength = 0;
  let lastRenderedPercent = -1;
  state.applicationUpdate = {
    phase: "downloading",
    message: `正在下载并安装 EnvNexus AI ${update.version}…`,
    availableVersion: update.version,
    progressPercent: 0,
  };
  state.error = undefined;
  render();
  try {
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        contentLength = event.data.contentLength ?? 0;
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
      }
      const percent =
        contentLength > 0
          ? Math.min(100, (downloaded / contentLength) * 100)
          : event.event === "Finished"
            ? 100
            : 3;
      const rounded = Math.floor(percent);
      if (rounded === lastRenderedPercent && event.event !== "Finished") return;
      lastRenderedPercent = rounded;
      state.applicationUpdate = {
        phase: "downloading",
        message:
          event.event === "Finished"
            ? "下载完成，正在验证签名并启动安装程序…"
            : `正在下载并安装 EnvNexus AI ${update.version}…`,
        availableVersion: update.version,
        progressPercent: percent,
      };
      render();
    });
    await relaunch();
  } catch (error) {
    state.applicationUpdate = {
      phase: "error",
      message: `更新失败：${String(error)}`,
      availableVersion: update.version,
    };
    render();
  }
}

async function loadBackups(): Promise<void> {
  try {
    state.backups = await backend.listEnvironmentBackups();
  } catch (error) {
    state.error = `读取环境备份失败：${String(error)}`;
  }
  render();
}

async function loadLogs(): Promise<void> {
  try {
    state.logs = await backend.recentOperationLogs();
  } catch (error) {
    state.error = `读取操作日志失败：${String(error)}`;
  }
  render();
}

async function applyPendingPlan(): Promise<void> {
  const plan = state.pendingPlan;
  if (!plan || state.applying) return;
  state.applying = true;
  state.error = undefined;
  render();
  try {
    const result = await backend.applyPlan(plan.id, plan.confirmationToken);
    state.notice = result.message;
    state.pendingPlan = undefined;
    state.progress = undefined;
    state.scanStale = true;
    try {
      state.terminalCommands = await backend.terminalCommandsStatus();
    } catch {
      // 操作已提交，终端命令状态可在命令说明页重新读取。
    }
  } catch (error) {
    state.error = `操作失败：${String(error)}`;
  } finally {
    state.applying = false;
    render();
  }
}

async function loadTerminalCommands(shouldRender = true): Promise<void> {
  try {
    state.terminalCommands = await backend.terminalCommandsStatus();
  } catch (error) {
    state.error = `读取终端命令状态失败：${String(error)}`;
  }
  if (shouldRender) render();
}

function render(): void {
  storeNavigation({
    view: state.view,
    selectedToolId: state.selectedToolId,
    selectedAiProviderId: state.selectedAiProviderId,
  });
  const root = document.querySelector<HTMLElement>("#app");
  if (!root) return;
  const preservedScrollTop = readMainScrollPosition(
    root,
    lastRenderedView === state.view,
  );
  root.innerHTML = renderShell();
  localizeUi(root, state.appPreferences?.language ?? "zh-CN");
  bindEvents(root);
  lastRenderedView = state.view;
  restoreMainScrollPosition(root, preservedScrollTop);
}

async function runScan(): Promise<void> {
  if (state.scanning) return;
  state.scanning = true;
  state.error = undefined;
  render();
  try {
    state.scan = await backend.scanEnvironment();
    state.scanStale = false;
  } catch (error) {
    state.error = `环境扫描失败：${String(error)}`;
  } finally {
    state.scanning = false;
    render();
  }
}

async function handleTrayAction(action: TrayAction): Promise<void> {
  if (action.kind === "scan") {
    state.view = "diagnostics";
    state.selectedToolId = undefined;
    render();
    await runScan();
    await loadLogs();
    return;
  }
  if (action.kind === "selectAiProvider") {
    try {
      state.aiSettings = await backend.aiSettings();
      state.selectedAiProviderId = action.providerId;
      const provider = state.aiSettings.providers.find(
        (candidate) => candidate.id === action.providerId,
      );
      state.notice = `托盘已将当前诊断 AI 切换为 ${provider?.displayName ?? action.providerId} / ${provider?.selectedModel ?? ""}。`;
      state.error = undefined;
    } catch (error) {
      state.error = `无法同步托盘 AI 厂商切换：${String(error)}`;
    }
    render();
    return;
  }
  if (action.kind === "openTool") {
    try {
      await ensureToolContext();
    } catch (error) {
      state.error = `无法读取工具目录：${String(error)}`;
      state.view = "tools";
      render();
      return;
    }
    state.view = "tool-detail";
    state.selectedToolId = action.toolId;
    render();
    return;
  }
  if (action.kind === "previewSwitch") {
    try {
      await ensureToolContext();
    } catch (error) {
      state.error = `无法读取工具目录：${String(error)}`;
      state.view = "tools";
      render();
      return;
    }
    state.view = "tool-detail";
    state.selectedToolId = action.toolId;
    state.error = undefined;
    render();
    try {
      state.pendingPlan = await backend.previewSwitch(
        action.toolId,
        action.installationPath,
      );
    } catch (error) {
      state.error = `无法生成切换计划：${String(error)}`;
    }
    render();
    return;
  }
  if (
    action.kind === "openDiagnostic" ||
    action.kind === "previewDiagnosticRepair"
  ) {
    try {
      await ensureToolContext();
      state.view = "diagnostics";
      state.selectedToolId = undefined;
      state.error = undefined;
      state.pendingPlan = undefined;
      state.diagnosticGuidance = undefined;
      render();
      if (action.kind === "openDiagnostic") {
        state.diagnosticGuidance = await backend.diagnosticGuidance(action.issueCode);
      } else {
        state.pendingPlan = await backend.previewDiagnosticRepair(action.issueCode);
      }
      await loadLogs();
    } catch (error) {
      state.error =
        action.kind === "openDiagnostic"
          ? `无法打开诊断详情：${String(error)}`
          : `无法生成诊断修复计划：${String(error)}`;
    }
    render();
    return;
  }
  if (action.kind === "navigate") {
    state.view = action.view;
    state.selectedToolId = undefined;
    render();
    if (action.view === "diagnostics") await loadLogs();
  }
}

export async function startApp(): Promise<void> {
  applyTheme(getStoredTheme());
  try {
    await listen<OperationProgress>("operation-progress", (event) => {
      state.progress = event.payload;
      if (state.applying) render();
    });
    await listen<TrayAction>("tray-action", (event) => {
      void handleTrayAction(event.payload);
    });
  } catch {
    // 纯浏览器开发环境没有 Tauri 事件桥；事件订阅失败不应阻止界面渲染。
  }
  render();
  try {
    const context = ensureToolContext();
    const [
      bootstrap,
      appPreferences,
      trayReady,
      aiSettings,
      terminalCommands,
    ] = await Promise.all([
      backend.bootstrap(),
      backend.appPreferences(),
      backend.trayReady(),
      backend.aiSettings(),
      backend.terminalCommandsStatus(),
    ]);
    await context;
    state.bootstrap = bootstrap;
    state.appPreferences = appPreferences;
    state.trayReady = trayReady;
    state.aiSettings = aiSettings;
    state.terminalCommands = terminalCommands;
    if (
      !state.selectedAiProviderId ||
      !aiSettings.providers.some(
        (provider) => provider.id === state.selectedAiProviderId,
      )
    ) {
      state.selectedAiProviderId =
        aiSettings.activeProviderId ?? aiSettings.providers[0]?.id;
    }
    if (
      state.view === "tool-detail" &&
      !state.toolDefinitions.some(
        (tool) => tool.id === state.selectedToolId,
      )
    ) {
      state.view = "tools";
      state.selectedToolId = undefined;
    }
    render();
    if (appPreferences.startMinimized) {
      await backend.hideToTray();
    }
  } catch (error) {
    state.error = `无法连接 EnvNexus AI 后端：${String(error)}`;
    render();
  }
}
