import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(join(process.cwd(), "src", "app.ts"), "utf8");
const tauriConfig = readFileSync(
  join(process.cwd(), "src-tauri", "tauri.conf.json"),
  "utf8",
);
const capability = readFileSync(
  join(process.cwd(), "src-tauri", "capabilities", "default.json"),
  "utf8",
);

describe("signed GitHub application updater", () => {
  it("checks on startup, supports manual retry, and confirms before installing", () => {
    expect(appSource).toContain('querySelector("#check-app-update")');
    expect(appSource).toContain("checkForApplicationUpdate()");
    expect(appSource).toContain("window.confirm(");
    expect(appSource).toContain("update.downloadAndInstall");

    const startup = appSource.slice(appSource.indexOf("export async function startApp"));
    expect(startup).toContain(
      "checkForApplicationUpdate({ automatic: true })",
    );
  });

  it("shows a green settings indicator that turns red for an available update", () => {
    expect(appSource).toContain("nav-update-dot");
    expect(appSource).toContain(
      'updateAvailable ? "available" : "current"',
    );
    expect(appSource).toContain("发现新版本");
  });

  it("uses signed releases and preserves the interactive installer directory flow", () => {
    const config = JSON.parse(tauriConfig) as {
      bundle: {
        createUpdaterArtifacts: boolean;
        windows: {
          nsis: {
            installMode: string;
            template?: string;
          };
        };
      };
      plugins: {
        updater: {
          pubkey: string;
          endpoints: string[];
          windows: { installMode: string };
        };
      };
    };
    const permissions = JSON.parse(capability) as { permissions: string[] };

    expect(config.bundle.createUpdaterArtifacts).toBe(true);
    expect(config.bundle.windows.nsis.installMode).toBe("currentUser");
    expect(config.bundle.windows.nsis.template).toBeUndefined();
    expect(config.plugins.updater.pubkey.length).toBeGreaterThan(100);
    expect(config.plugins.updater.endpoints).toEqual([
      "https://github.com/PuppetWen/EnvNexus-AI/releases/latest/download/latest.json",
    ]);
    expect(config.plugins.updater.windows.installMode).toBe("passive");
    expect(permissions.permissions).toContain("updater:default");
  });
});
