import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(join(process.cwd(), "src", "app.ts"), "utf8");
const bridgeSource = readFileSync(join(process.cwd(), "src", "bridge.ts"), "utf8");
const backendSource = readFileSync(
  join(process.cwd(), "src-tauri", "src", "lib.rs"),
  "utf8",
);

describe("close-to-tray behavior", () => {
  it("exposes and persists the explicit keep-running option", () => {
    expect(appSource).toContain('option value="minimizeToTray"');
    expect(appSource).toContain("关闭窗口后驻留系统托盘（不退出）");
    expect(appSource).toContain("backend.saveAppPreferences(preferences)");
    expect(bridgeSource).toContain('invoke("save_app_preferences"');
  });

  it("prevents close, hides the window, and keeps an explicit tray exit", () => {
    expect(backendSource).toContain("WindowEvent::CloseRequested");
    expect(backendSource).toContain("CloseBehavior::MinimizeToTray");
    expect(backendSource).toContain("api.prevent_close()");
    expect(backendSource).toContain("window.hide()");
    expect(backendSource).toContain('"tray_exit" => app.exit(0)');
    expect(backendSource).toContain("TrayIconEvent::DoubleClick");
  });

  it("uses the WebView2 low-memory target only while inactive", () => {
    expect(backendSource).toContain("WindowEvent::Focused(focused)");
    expect(backendSource).toContain("COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW");
    expect(backendSource).toContain("COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL");
    expect(backendSource).toContain("SetMemoryUsageTargetLevel");
  });
});
