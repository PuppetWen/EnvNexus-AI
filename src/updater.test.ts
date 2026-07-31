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
const updaterBackend = readFileSync(
  join(
    process.cwd(),
    "src-tauri",
    "src",
    "application_update.rs",
  ),
  "utf8",
);
const releaseScript = readFileSync(
  join(process.cwd(), "scripts", "Prepare-Release.ps1"),
  "utf8",
);

describe("signed GitHub application updater", () => {
  it("checks on startup and hands a confirmed update to the transactional backend", () => {
    expect(appSource).toContain('querySelector("#check-app-update")');
    expect(appSource).toContain("checkForApplicationUpdate()");
    expect(appSource).toContain("window.confirm(");
    expect(appSource).toContain("backend.prepareApplicationUpdate(request)");
    expect(appSource).toContain("backend.launchApplicationUpdate");
    expect(appSource).toContain("await exit(0)");
    expect(appSource).not.toContain("update.downloadAndInstall");

    const startup = appSource.slice(appSource.indexOf("export async function startApp"));
    expect(startup).toContain(
      "checkForApplicationUpdate({ automatic: true })",
    );
    expect(startup).toContain("confirmApplicationUpdateStarted()");
  });

  it("shows a themed brand indicator that turns red for an available update", () => {
    expect(appSource).toContain("brand-update-indicator");
    expect(appSource).toContain("brand-update-tooltip");
    expect(appSource).toContain('data-nav="settings"');
    expect(appSource).not.toContain("nav-update-dot");
    expect(appSource).toContain(
      'updateAvailable ? "available" : updateBusy ? "busy" : "current"',
    );
    expect(appSource).toContain("发现新版本");
  });

  it("uses check-only updater permission and preserves the installer directory flow", () => {
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
    expect(permissions.permissions).toContain("updater:allow-check");
    expect(permissions.permissions).toContain("process:allow-exit");
    expect(permissions.permissions).not.toContain("updater:default");
  });

  it("implements resumable verification, silent installed updates, portable replacement, and rollback", () => {
    expect(updaterBackend).toContain("DOWNLOAD_MAX_ATTEMPTS: u32 = 8");
    expect(updaterBackend).toContain('header::RANGE');
    expect(updaterBackend).toContain("verify_minisign");
    expect(updaterBackend).toContain('args(["/S", "/UPDATE", "/NS"])');
    expect(updaterBackend).toContain("apply_portable_update");
    expect(updaterBackend).toContain("restore_backup");
    expect(updaterBackend).toContain("waiting_for_new_version");
    expect(updaterBackend).toContain("schedule_committed_cleanup");
  });

  it("publishes signed installer and portable metadata with SHA-256 hashes", () => {
    expect(releaseScript).toContain("signer sign $PortablePath");
    expect(releaseScript).toContain("$PortableSignaturePath");
    expect(releaseScript).toContain("portable = [ordered]@{");
    expect(releaseScript).toContain("sha256 = $PortableHash");
    expect(releaseScript).toContain("sha256 = $InstallerHash");
  });
});
