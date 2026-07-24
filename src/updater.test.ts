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
  it("only checks after the user clicks and confirms before installing", () => {
    expect(appSource).toContain('querySelector("#check-app-update")');
    expect(appSource).toContain("checkForApplicationUpdate()");
    expect(appSource).toContain("window.confirm(");
    expect(appSource).toContain("update.downloadAndInstall");

    const startup = appSource.slice(appSource.indexOf("export async function startApp"));
    expect(startup).not.toContain("checkForApplicationUpdate()");
  });

  it("uses an HTTPS GitHub release endpoint and signed updater permissions", () => {
    const config = JSON.parse(tauriConfig) as {
      bundle: { createUpdaterArtifacts: boolean };
      plugins: { updater: { pubkey: string; endpoints: string[] } };
    };
    const permissions = JSON.parse(capability) as { permissions: string[] };

    expect(config.bundle.createUpdaterArtifacts).toBe(true);
    expect(config.plugins.updater.pubkey.length).toBeGreaterThan(100);
    expect(config.plugins.updater.endpoints).toEqual([
      "https://github.com/PuppetWen/EnvNexus-AI/releases/latest/download/latest.json",
    ]);
    expect(permissions.permissions).toContain("updater:default");
  });
});
