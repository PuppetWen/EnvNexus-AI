import { beforeEach, describe, expect, it, vi } from "vitest";
import type { OperationPlan } from "./types";
import { previewInstallationEnvironment } from "./install-followup";

const backendMock = vi.hoisted(() => ({
  bootstrap: vi.fn(),
  appPreferences: vi.fn(),
  trayReady: vi.fn(),
  aiSettings: vi.fn(),
  terminalCommandsStatus: vi.fn(),
  toolDefinitions: vi.fn(),
  toolRootPreferences: vi.fn(),
  cachedEnvironmentScan: vi.fn(),
  confirmApplicationUpdateStarted: vi.fn(),
  fetchVersions: vi.fn(),
  previewInstall: vi.fn(),
  applyPlan: vi.fn(),
  refreshEnvironmentScan: vi.fn(),
  previewSwitch: vi.fn(),
}));

vi.mock("./bridge", () => ({ backend: backendMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn().mockResolvedValue(null) }));

const installPath = "E:\\Environment\\java\\21.0.8";
const installPlan: OperationPlan = {
  id: "install-java",
  toolId: "java",
  actionKind: "install",
  title: "安装 JDK 21.0.8",
  summary: `安装到 ${installPath}`,
  createdAt: "2026-10-07T12:00:00Z",
  expiresAt: "2026-10-07T12:20:00Z",
  confirmationToken: "install-token",
  requiresElevation: false,
  warnings: [],
  conflicts: [],
  environmentDiffs: [],
  steps: [{ kind: "download", description: "下载安装包", destructive: false }],
};
const switchPlan: OperationPlan = {
  ...installPlan,
  id: "switch-java",
  actionKind: "switch",
  title: "切换 JDK 默认版本",
  confirmationToken: "switch-token",
  environmentDiffs: [{
    scope: "user",
    variable: "JAVA_HOME",
    after: installPath,
    added: [installPath],
    removed: [],
  }],
};
const installResult = {
  operationId: installPlan.id,
  status: "committed",
  message: "java 21.0.8 安装完成",
  installationPath: installPath,
};

function click(selector: string): void {
  const element = document.querySelector<HTMLElement>(selector);
  expect(element, selector).not.toBeNull();
  element!.click();
}

async function beginInstallation(): Promise<void> {
  const { startApp } = await import("./app");
  await startApp();
  click("[data-fetch-versions]");
  await vi.waitFor(() => expect(document.querySelector("[data-install-version]")).not.toBeNull());
  click("[data-install-version]");
  await vi.waitFor(() => expect(document.querySelector("#confirm-plan")).not.toBeNull());
  click("#confirm-plan");
}

beforeEach(() => {
  vi.resetModules();
  vi.resetAllMocks();
  window.localStorage.clear();
  window.localStorage.setItem("envnexus-ai.navigation", JSON.stringify({
    schemaVersion: 1,
    view: "tool-detail",
    selectedToolId: "java",
  }));
  document.body.innerHTML = '<div id="app"></div>';
  backendMock.bootstrap.mockResolvedValue({
    appVersion: "0.1.4", dataRoot: "E:\\EnvNexusAIData", installationKind: "portable",
  });
  backendMock.appPreferences.mockResolvedValue({ language: "zh-CN", startMinimized: false });
  backendMock.trayReady.mockResolvedValue(true);
  backendMock.aiSettings.mockResolvedValue({ providers: [] });
  backendMock.terminalCommandsStatus.mockResolvedValue({ enabledInUserPath: false });
  backendMock.toolDefinitions.mockResolvedValue([{
    id: "java", displayName: "JDK", category: "Java", icon: "Coffee",
    capabilities: { install: true, switchDefault: true, repair: true, uninstall: true },
  }]);
  backendMock.toolRootPreferences.mockResolvedValue({ roots: { java: "E:\\Environment" } });
  backendMock.cachedEnvironmentScan.mockResolvedValue(null);
  backendMock.confirmApplicationUpdateStarted.mockResolvedValue(false);
  backendMock.fetchVersions.mockResolvedValue({
    toolId: "java", sourceName: "Adoptium", fetchedAt: installPlan.createdAt, cached: false,
    versions: [{ version: "21.0.8", channel: "LTS", architecture: "x64", downloadUrl: "https://api.adoptium.net/21.0.8.zip" }],
  });
  backendMock.previewInstall.mockResolvedValue(installPlan);
  backendMock.applyPlan.mockResolvedValue(installResult);
  backendMock.refreshEnvironmentScan.mockResolvedValue({
    tools: [], issues: [], versionManagers: [], scanFinishedAt: installPlan.createdAt,
  });
  backendMock.previewSwitch.mockResolvedValue(switchPlan);
});

describe("installation environment confirmation", () => {
  it("opens the environment diff after installing and can keep installation without writing it", async () => {
    await beginInstallation();
    await vi.waitFor(() => expect(document.querySelector(".plan-modal")?.textContent).toContain("JAVA_HOME"));

    expect(backendMock.previewSwitch).toHaveBeenCalledWith("java", installPath);
    expect(backendMock.applyPlan).toHaveBeenCalledExactlyOnceWith("install-java", "install-token");
    expect(document.querySelector(".plan-modal")?.textContent).toContain("安装已完成");
    const cancelButton = document.querySelector<HTMLButtonElement>(".plan-footer #cancel-plan")!;
    expect(cancelButton.textContent).toBe("仅保留安装");
    cancelButton.click();

    expect(document.querySelector(".plan-modal")).toBeNull();
    expect(document.querySelector(".notice-banner")?.textContent).toContain("尚未应用环境变量");
    expect(backendMock.applyPlan).toHaveBeenCalledTimes(1);
  });

  it("writes the environment only after the follow-up plan receives its own confirmation", async () => {
    backendMock.applyPlan
      .mockResolvedValueOnce(installResult)
      .mockResolvedValueOnce({ operationId: "switch-java", status: "committed", message: "默认版本已切换" });
    await beginInstallation();
    await vi.waitFor(() => expect(document.querySelector(".plan-modal")?.textContent).toContain("JAVA_HOME"));
    click("#confirm-plan");
    await vi.waitFor(() => expect(document.querySelector(".notice-banner")?.textContent).toContain("默认版本已切换"));

    expect(backendMock.applyPlan).toHaveBeenNthCalledWith(2, "switch-java", "switch-token");
    expect(backendMock.previewSwitch).toHaveBeenCalledTimes(1);
    expect(document.querySelector(".plan-modal")).toBeNull();
  });

  it("keeps installation success when generating the environment plan fails", async () => {
    backendMock.previewSwitch.mockRejectedValue(new Error("安装目录无法访问"));
    await beginInstallation();
    await vi.waitFor(() => expect(document.querySelector(".notice-banner")?.textContent).toContain("环境尚未配置"));

    expect(document.querySelector(".notice-banner")?.textContent).toContain("安装完成");
    expect(document.querySelector(".error-banner")).toBeNull();
    expect(document.querySelector(".plan-modal")).toBeNull();
    expect(backendMock.applyPlan).toHaveBeenCalledTimes(1);
  });

  it("can preview environment configuration even if inventory refresh fails", async () => {
    backendMock.refreshEnvironmentScan.mockRejectedValue(new Error("扫描失败"));
    await beginInstallation();
    await vi.waitFor(() => expect(document.querySelector(".plan-modal")?.textContent).toContain("JAVA_HOME"));
    expect(backendMock.previewSwitch).toHaveBeenCalledWith("java", installPath);
    expect(document.querySelector(".error-banner")).toBeNull();
  });

  it("keeps completed progress accessible when navigating during inventory refresh", async () => {
    let finishRefresh!: (scan: object) => void;
    backendMock.refreshEnvironmentScan.mockReturnValue(new Promise((resolve) => {
      finishRefresh = resolve;
    }));
    await beginInstallation();
    await vi.waitFor(() => expect(backendMock.refreshEnvironmentScan).toHaveBeenCalledTimes(1));
    click('[data-nav="tools"]');

    expect(document.querySelector("[data-operation-percent]")?.textContent).toBe("100.0%");
    click("#show-operation-progress");
    expect(document.querySelector(".plan-modal h2")?.textContent).toBe(installPlan.title);
    expect(document.querySelector(".plan-modal [data-operation-percent]")?.textContent).toBe("100.0%");
    expect(document.querySelector("#confirm-plan")).toBeNull();

    finishRefresh({ tools: [], issues: [], versionManagers: [], scanFinishedAt: installPlan.createdAt });
    await vi.waitFor(() => expect(document.querySelector(".plan-modal")?.textContent).toContain("JAVA_HOME"));
    expect(document.querySelector("[data-operation-progress]")).toBeNull();
  });

  it("does not activate repairs, unknown plans, failed installs, or installs without a path", async () => {
    const preview = vi.fn().mockResolvedValue(switchPlan);
    for (const actionKind of ["repair", undefined] as const) {
      const result = await previewInstallationEnvironment({ ...installPlan, actionKind }, installResult, preview);
      expect(result.pendingPlan).toBeUndefined();
    }
    await previewInstallationEnvironment(installPlan, { ...installResult, status: "failed" }, preview);
    const missingPath = await previewInstallationEnvironment(
      installPlan, { ...installResult, installationPath: undefined }, preview,
    );
    expect(missingPath.notice).toContain("环境尚未配置");
    expect(preview).not.toHaveBeenCalled();
  });

  it("offers a separate environment plan after Python repair adds pip scripts", async () => {
    const pythonPath = "E:\\Environment\\python\\3.15.0";
    const plan = { ...installPlan, actionKind: "repair" as const, toolId: "python" };
    const preview = vi.fn().mockResolvedValue({ ...switchPlan, toolId: "python" });
    const result = await previewInstallationEnvironment(
      plan, { ...installResult, installationPath: pythonPath }, preview,
    );
    expect(preview).toHaveBeenCalledExactlyOnceWith("python", pythonPath);
    expect(result.pendingPlan?.toolId).toBe("python");
    expect(result.notice).toContain("请确认默认版本计划");
  });
});
