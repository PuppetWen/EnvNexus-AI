import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(join(process.cwd(), "src", "app.ts"), "utf8");
const bridgeSource = readFileSync(join(process.cwd(), "src", "bridge.ts"), "utf8");
const scannerSource = readFileSync(
  join(process.cwd(), "src-tauri", "src", "scanner.rs"),
  "utf8",
);
const cliSource = readFileSync(
  join(process.cwd(), "src-tauri", "src", "cli.rs"),
  "utf8",
);

describe("whole-computer environment discovery", () => {
  it("indexes every fixed disk and classifies matching tool executables", () => {
    expect(scannerSource).toContain("GetLogicalDriveStringsW");
    expect(scannerSource).toContain("GetDriveTypeW");
    expect(scannerSource).toContain("discover_disk_index_in_roots");
    expect(scannerSource).toContain('"全机磁盘扫描"');
    expect(scannerSource).toContain("tool-executable-discovery.json");
  });

  it("refreshes the cached inventory after roots and operations change", () => {
    expect(bridgeSource).toContain('invoke("refresh_environment_scan")');
    expect(appSource).toContain("runScan({ incremental: true })");
    expect(appSource).toContain("backend.refreshEnvironmentScan()");
    expect(cliSource).toContain("scanner::refresh(registry, data_root)");
  });

  it("reuses fingerprinted version probes only for incremental refreshes", () => {
    expect(scannerSource).toContain("tool-version-probes.json");
    expect(scannerSource).toContain("ToolProbeFingerprint");
    expect(scannerSource).toContain("reuse_cached: !force_disk_discovery");
    expect(scannerSource).toContain("cacheable_probe_candidate");
    expect(scannerSource).toContain("executable_modified_millis");
    expect(scannerSource).toContain("if force_disk_discovery");
  });
});
