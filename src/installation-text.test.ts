import { beforeEach, describe, expect, it, vi } from "vitest";
import type { OperationPlan, OperationProgress } from "./types";

const tools = ["python", "java", "go", "rust", "node", "git", "android-sdk", "android-ndk", "gradle", "cmake", "adb", "maven", "dotnet", "ruby", "php"];
const mocks = vi.hoisted(() => ({
  bootstrap: vi.fn(), appPreferences: vi.fn(), trayReady: vi.fn(), aiSettings: vi.fn(),
  terminalCommandsStatus: vi.fn(), toolDefinitions: vi.fn(), toolRootPreferences: vi.fn(),
  cachedEnvironmentScan: vi.fn(), confirmApplicationUpdateStarted: vi.fn(), fetchVersions: vi.fn(),
  previewInstall: vi.fn(), applyPlan: vi.fn(),
  events: new Map<string, (event: { payload: OperationProgress }) => void>(),
}));
vi.mock("./bridge", () => ({ backend: mocks }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (name, callback) => {
  mocks.events.set(name, callback);
  return () => {};
}) }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn().mockResolvedValue(null) }));

// These are text fixtures, not filesystem destinations. Literal entity syntax
// and markup must survive as data, alongside real Windows path separators.
const path = 'E:\\开发 & Tools\\日本語-é\\<package>\\"quoted"\\literal&amp;\\bin';
const oldPath = "\\\\server\\共享\\旧版\\bin";
const message = `正在验证 ${path}\n保留大小写 Git / JAVA_HOME / Php；错误 <file> & "引号"`;
const createdAt = "2026-10-18T05:00:00Z";

function click(selector: string): void {
  const element = document.querySelector<HTMLElement>(selector);
  expect(element, selector).not.toBeNull();
  element!.click();
}

beforeEach(() => {
  vi.resetModules();
  vi.resetAllMocks();
  mocks.events.clear();
  window.localStorage.clear();
  document.body.innerHTML = '<div id="app"></div>';
  mocks.bootstrap.mockResolvedValue({ appVersion: "0.1.9", dataRoot: path, installationKind: "portable" });
  mocks.appPreferences.mockResolvedValue({ language: "ja-JP", startMinimized: false });
  mocks.trayReady.mockResolvedValue(true);
  mocks.aiSettings.mockResolvedValue({ providers: [] });
  mocks.terminalCommandsStatus.mockResolvedValue({ enabledInUserPath: false });
  mocks.toolDefinitions.mockResolvedValue(tools.map((id) => ({
    id, displayName: id, category: "运行时", icon: "Terminal",
    capabilities: { install: true, switchDefault: true, repair: true, uninstall: true },
  })));
  mocks.toolRootPreferences.mockResolvedValue({ roots: Object.fromEntries(tools.map((id) => [id, path])), androidRoot: path });
  mocks.cachedEnvironmentScan.mockResolvedValue(null);
  mocks.confirmApplicationUpdateStarted.mockResolvedValue(false);
});

describe("installation text across all tool families", () => {
  it.each(tools)("preserves %s paths through confirmation, live progress, background and errors", async (toolId) => {
    window.localStorage.setItem("envnexus-ai.navigation", JSON.stringify({ schemaVersion: 1, view: "tool-detail", selectedToolId: toolId }));
    const plan: OperationPlan = {
      id: `install-${toolId}`, toolId, actionKind: "install", title: `安装 ${toolId}`,
      summary: message, createdAt, expiresAt: createdAt, confirmationToken: "preview-only",
      requiresElevation: false, conflicts: [], warnings: [message],
      environmentDiffs: [{ scope: "user", variable: "PATH", added: [path], removed: [oldPath], before: oldPath, after: path }],
      steps: [{ kind: "extract", description: message, destructive: false }],
    };
    mocks.fetchVersions.mockResolvedValue({ toolId, fetchedAt: createdAt, sourceName: "Text fixture", cached: false,
      versions: [{ version: "1.2.3", channel: "stable", architecture: "x86_64", downloadUrl: "https://example.com/package.zip" }],
    });
    mocks.previewInstall.mockResolvedValue(plan);
    let rejectInstall!: (reason: Error) => void;
    mocks.applyPlan.mockReturnValue(new Promise((_, reject) => { rejectInstall = reject; }));
    const { startApp } = await import("./app");
    await startApp();
    click("[data-fetch-versions]");
    await vi.waitFor(() => expect(document.querySelector("[data-install-version]")).not.toBeNull());
    click("[data-install-version]");
    await vi.waitFor(() => expect(document.querySelector("#confirm-plan")).not.toBeNull());
    expect(document.querySelector(".plan-header .path-context")?.textContent).toBe(message);
    expect(document.querySelector(".plan-warnings span")?.textContent).toBe(message);
    expect(document.querySelector(".plan-steps p")?.textContent).toBe(message);
    expect(document.querySelector(".diff-list .add")?.textContent).toBe(`+ ${path}`);
    expect(document.querySelector(".diff-list .remove")?.textContent).toBe(`- ${oldPath}`);
    expect(document.querySelector("package")).toBeNull();
    click("#confirm-plan");
    await vi.waitFor(() => expect(mocks.applyPlan).toHaveBeenCalledOnce());
    click("#show-operation-progress");
    const progress = { operationId: plan.id, phase: "verifying", message, percent: 71.5, receivedBytes: 0 };
    mocks.events.get("operation-progress")!({ payload: progress });
    const text = document.querySelector<HTMLElement>("[data-operation-message]")!;
    expect(text.textContent).toBe(message);
    expect(text.classList.contains("path-context")).toBe(true);
    expect(text.title).toBe(message);
    click("#minimize-operation");
    const background = document.querySelector<HTMLElement>("[data-background-operation] [data-operation-message]")!;
    expect(background.textContent).toBe(message);
    expect(background.classList.contains("path-context")).toBe(true);
    expect(background.title).toBe(message);
    const nextMessage = `${message}\n下一步：${oldPath}`;
    mocks.events.get("operation-progress")!({ payload: { ...progress, message: nextMessage } });
    expect(background.textContent).toBe(nextMessage);
    expect(background.title).toBe(nextMessage);
    click("#show-operation-progress");
    expect(document.querySelector("[data-operation-message]")?.textContent).toBe(nextMessage);
    rejectInstall(new Error(nextMessage));
    await vi.waitFor(() => expect(document.querySelector(".error-banner span")?.textContent).toBe(`操作失败：Error: ${nextMessage}`));
    expect(document.querySelector("package")).toBeNull();
    expect(document.querySelector(".error-banner")?.classList.contains("path-context")).toBe(true);
    expect(mocks.applyPlan).toHaveBeenCalledExactlyOnceWith(plan.id, plan.confirmationToken);
  });
});
