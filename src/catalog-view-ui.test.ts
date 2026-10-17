import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RemoteVersion, VersionCatalog } from "./types";

const backendMock = vi.hoisted(() => ({
  bootstrap: vi.fn(), appPreferences: vi.fn(), trayReady: vi.fn(), aiSettings: vi.fn(),
  terminalCommandsStatus: vi.fn(), toolDefinitions: vi.fn(), toolRootPreferences: vi.fn(),
  cachedEnvironmentScan: vi.fn(), confirmApplicationUpdateStarted: vi.fn(),
  fetchVersions: vi.fn(), previewInstall: vi.fn(),
}));
vi.mock("./bridge", () => ({ backend: backendMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn().mockResolvedValue(null) }));

function release(version: string, extra: Partial<RemoteVersion> = {}): RemoteVersion {
  return { version, channel: "stable", architecture: "x86_64", downloadUrl: `https://www.python.org/${version}.zip`, ...extra };
}
function catalog(toolId: string, versions: RemoteVersion[], fetchedAt = "2026-10-17T12:00:00Z"): VersionCatalog {
  return { toolId, sourceName: "Official releases", sourceUrl: "https://www.python.org/", fetchedAt, cached: false, versions };
}
const pythonVersions = [
  release("3.15.0"), release("3.16.0rc1", { channel: "preview" }), release("3.10.11"),
  release("2.7.18", { notesUrl: "https://www.python.org/downloads/release/python-2718/" }),
  release("2.3.7", { installUnavailableReason: "尚未支持旧式 EXE 安装器", notesUrl: "javascript:alert(1)" }),
  release("2.7.19", { downloadUrl: undefined, installUnavailableReason: "仅源码发行" }),
  release("2.0.1", { downloadUrl: undefined, channel: "archive" }),
];

function click(selector: string): void {
  const element = document.querySelector<HTMLElement>(selector);
  expect(element, selector).not.toBeNull();
  element!.click();
}
function input(selector: string, value: string): HTMLInputElement {
  const element = document.querySelector<HTMLInputElement>(selector)!;
  element.value = value;
  element.dispatchEvent(new Event("input", { bubbles: true }));
  return element;
}
function select(selector: string, value: string): void {
  const element = document.querySelector<HTMLSelectElement>(selector)!;
  element.value = value;
  element.dispatchEvent(new Event("change", { bubbles: true }));
}
function installable(checked: boolean): void {
  const element = document.querySelector<HTMLInputElement>("[data-catalog-installable]")!;
  element.checked = checked;
  element.dispatchEvent(new Event("change", { bubbles: true }));
}
function visibleVersions(): string[] {
  return Array.from(document.querySelectorAll<HTMLElement>("[data-catalog-version]"), (element) => element.dataset.catalogVersion!);
}
async function openCatalog(): Promise<void> {
  const { startApp } = await import("./app");
  await startApp();
  click("[data-fetch-versions]");
  await vi.waitFor(() => expect(visibleVersions()).toHaveLength(7));
}

beforeEach(() => {
  vi.resetModules();
  vi.resetAllMocks();
  window.localStorage.clear();
  window.localStorage.setItem("envnexus-ai.navigation", JSON.stringify({ schemaVersion: 1, view: "tool-detail", selectedToolId: "python" }));
  document.body.innerHTML = '<div id="app"></div>';
  backendMock.bootstrap.mockResolvedValue({ appVersion: "0.1.6", dataRoot: "E:\\TestData", installationKind: "portable" });
  backendMock.appPreferences.mockResolvedValue({ language: "zh-CN", startMinimized: false });
  backendMock.trayReady.mockResolvedValue(true);
  backendMock.aiSettings.mockResolvedValue({ providers: [] });
  backendMock.terminalCommandsStatus.mockResolvedValue({ enabledInUserPath: false });
  backendMock.toolDefinitions.mockResolvedValue(["python", "node"].map((id) => ({ id, displayName: id, category: "Tools", icon: "Code", capabilities: { install: true, switchDefault: true, repair: true, uninstall: true } })));
  backendMock.toolRootPreferences.mockResolvedValue({ roots: { python: "E:\\Environment", node: "E:\\Node" } });
  backendMock.cachedEnvironmentScan.mockResolvedValue(null);
  backendMock.confirmApplicationUpdateStarted.mockResolvedValue(false);
  backendMock.fetchVersions.mockImplementation(async (toolId: string) => toolId === "python" ? catalog("python", pythonVersions) : catalog("node", [release("22.0.0"), release("24.0.0")]));
  backendMock.previewInstall.mockResolvedValue({
    id: "python-plan", toolId: "python", actionKind: "install", title: "安装 Python", summary: "isolated plan",
    expiresAt: "2026-10-17T12:20:00Z", confirmationToken: "token", requiresElevation: false,
    warnings: [], steps: [], conflicts: [], environmentDiffs: [],
  });
});

describe("release catalog controls and installation guards", () => {
  it("shows every historical release with accurate counts, reasons, and safe official links", async () => {
    await openCatalog();
    expect(visibleVersions()).toEqual(["3.16.0rc1", "3.15.0", "3.10.11", "2.7.19", "2.7.18", "2.3.7", "2.0.1"]);
    expect(document.querySelector("[data-catalog-count]")?.textContent).toBe("显示 7 / 共 7 个版本 · 4 个可安装");
    for (const version of ["2.3.7", "2.7.19", "2.0.1"]) {
      expect(document.querySelector<HTMLButtonElement>(`[data-catalog-version="${version}"] button`)?.disabled).toBe(true);
    }
    expect(document.querySelector('[data-catalog-version="2.0.1"]')?.textContent).toContain("无 Windows 安装包");
    const official = document.querySelector<HTMLAnchorElement>('[data-catalog-version="2.7.18"] a')!;
    expect(official.href).toBe("https://www.python.org/downloads/release/python-2718/");
    expect(official.target).toBe("_blank");
    expect(official.rel).toBe("noopener noreferrer");
    expect(document.querySelector('[data-catalog-version="2.3.7"] a')).toBeNull();
  });

  it("combines filters and updates the list without replacing or unfocusing the search field", async () => {
    await openCatalog();
    const search = document.querySelector<HTMLInputElement>("[data-catalog-query]")!;
    search.focus();
    input("[data-catalog-query]", "2.7");
    expect(document.activeElement).toBe(search);
    expect(visibleVersions()).toEqual(["2.7.19", "2.7.18"]);
    installable(true);
    select("[data-catalog-channel]", "stable");
    expect(visibleVersions()).toEqual(["2.7.18"]);
    expect(document.querySelector("[data-catalog-count]")?.textContent).toBe("显示 1 / 共 7 个版本 · 4 个可安装");
    input("[data-catalog-query]", "");
    select("[data-catalog-channel]", "preview");
    expect(visibleVersions()).toEqual(["3.16.0rc1"]);
    installable(false);
    select("[data-catalog-channel]", "archive");
    expect(visibleVersions()).toEqual(["2.0.1"]);
    input("[data-catalog-query]", "no-such-release");
    expect(visibleVersions()).toEqual([]);
    click("[data-reset-catalog]");
    expect(visibleVersions()).toHaveLength(7);
  });

  it("preserves each tool's filters through refreshes and navigation", async () => {
    await openCatalog();
    input("[data-catalog-query]", "2.7");
    installable(true);
    select("[data-catalog-sort]", "version-asc");
    backendMock.fetchVersions.mockResolvedValueOnce(catalog("python", [...pythonVersions, release("2.7.17")], "2026-10-17T12:01:00Z"));
    click("[data-fetch-versions]");
    await vi.waitFor(() => expect(visibleVersions()).toEqual(["2.7.17", "2.7.18"]));
    expect(backendMock.fetchVersions).toHaveBeenLastCalledWith("python", true);
    expect(document.querySelector<HTMLInputElement>("[data-catalog-query]")?.value).toBe("2.7");
    expect(document.querySelector("[data-catalog-count]")?.textContent).toBe("显示 2 / 共 8 个版本 · 5 个可安装");
    click('[data-nav="tools"]');
    click('[data-open-tool="node"]');
    click("[data-fetch-versions]");
    await vi.waitFor(() => expect(visibleVersions()).toEqual(["24.0.0", "22.0.0"]));
    expect(document.querySelector<HTMLInputElement>("[data-catalog-query]")?.value).toBe("");
    expect(document.querySelector<HTMLInputElement>("[data-catalog-installable]")?.checked).toBe(false);
    input("[data-catalog-query]", "24");
    click('[data-nav="tools"]');
    click('[data-open-tool="python"]');
    expect(visibleVersions()).toEqual(["2.7.17", "2.7.18"]);
    expect(document.querySelector<HTMLSelectElement>("[data-catalog-sort]")?.value).toBe("version-asc");
  });

  it("rejects unavailable releases in the event handler even if a click is dispatched directly", async () => {
    await openCatalog();
    document.querySelector('[data-catalog-version="2.3.7"] button')!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(backendMock.previewInstall).not.toHaveBeenCalled();
    expect(document.querySelector(".error-banner")?.textContent).toContain("尚未支持旧式 EXE 安装器");
    document.querySelector('[data-catalog-version="2.0.1"] button')!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(backendMock.previewInstall).not.toHaveBeenCalled();
    expect(document.querySelector(".error-banner")?.textContent).toContain("无 Windows 安装包");
  });

  it("lets a supported historical release generate the normal installation plan", async () => {
    await openCatalog();
    click('[data-catalog-version="2.7.18"] button');
    await vi.waitFor(() => expect(document.querySelector("#confirm-plan")).not.toBeNull());
    expect(backendMock.previewInstall).toHaveBeenCalledExactlyOnceWith("python", "2.7.18", "E:\\Environment");
  });

  it("keeps the installation-directory guard for an otherwise supported release", async () => {
    backendMock.toolRootPreferences.mockResolvedValue({ roots: {} });
    await openCatalog();
    const button = document.querySelector<HTMLButtonElement>('[data-catalog-version="2.7.18"] button')!;
    expect(button.disabled).toBe(true);
    button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(backendMock.previewInstall).not.toHaveBeenCalled();
    expect(document.querySelector(".error-banner")?.textContent).toContain("请先在工具详情页上方设置默认安装目录");
  });
});
