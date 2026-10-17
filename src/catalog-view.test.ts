import { describe, expect, it } from "vitest";
import {
  catalogView,
  compareReleaseVersions,
  defaultCatalogView,
  installUnavailableReason,
  isPreviewRelease,
  officialReleaseUrl,
} from "./catalog-view";
import type { RemoteVersion } from "./types";

function release(version: string, extra: Partial<RemoteVersion> = {}): RemoteVersion {
  return { version, channel: "stable", architecture: "x86_64", downloadUrl: `https://www.python.org/${version}.zip`, ...extra };
}

describe("release catalog views", () => {
  it("defaults to all history, previews, and source-only releases without changing the source array", () => {
    const releases = [release("2.7.18"), release("3.9.20"), release("3.15.0rc1"), release("1.6", { downloadUrl: undefined })];
    const original = [...releases];
    const view = catalogView(releases, defaultCatalogView());
    expect(view.versions.map((item) => item.version)).toEqual(["3.15.0rc1", "3.9.20", "2.7.18", "1.6"]);
    expect([view.filteredCount, view.totalCount, view.installableCount]).toEqual([4, 4, 3]);
    expect(releases).toEqual(original);
    const otherTool = defaultCatalogView();
    otherTool.query = "node";
    expect(defaultCatalogView().query).toBe("");
  });

  it("combines keywords, release channel, and installability rather than hiding old versions", () => {
    const releases = [
      release("2.7.18"), release("2.7.17"), release("2.7.18rc1"),
      release("2.7.16", { installUnavailableReason: "仅源码发行" }), release("3.15.0"),
    ];
    const preferences = { ...defaultCatalogView(), query: " 2.7 X86_64 ", channel: "stable" as const, installableOnly: true, sort: "version-asc" as const };
    expect(catalogView(releases, preferences).versions.map((item) => item.version)).toEqual(["2.7.17", "2.7.18"]);
    expect(catalogView(releases, { ...preferences, query: "仅源码", installableOnly: false }).versions.map((item) => item.version)).toEqual(["2.7.16"]);
    expect(catalogView(releases, { ...preferences, channel: "preview" }).versions.map((item) => item.version)).toEqual(["2.7.18rc1"]);
  });

  it("treats an explicit unsupported reason and a missing download as installation blockers", () => {
    expect(installUnavailableReason(release("3.15.0", { installUnavailableReason: null }))).toBeUndefined();
    expect(installUnavailableReason(release("2.3.7", { installUnavailableReason: "尚未支持旧安装器" }))).toBe("尚未支持旧安装器");
    expect(installUnavailableReason(release("1.6", { downloadUrl: undefined }))).toBe("无 Windows 安装包");
    expect(installUnavailableReason(release("2.0", { installUnavailableReason: "  " }))).toBeTruthy();
  });

  it("keeps archive records in their own filter rather than treating them as stable releases", () => {
    const releases = [release("3.15.0"), release("2.0", { channel: "archive", downloadUrl: undefined }), release("3.16.0rc1")];
    expect(catalogView(releases, { ...defaultCatalogView(), channel: "stable" }).versions.map((item) => item.version)).toEqual(["3.15.0"]);
    expect(catalogView(releases, { ...defaultCatalogView(), channel: "archive" }).versions.map((item) => item.version)).toEqual(["2.0"]);
  });

  it("sorts numeric version parts and prerelease revisions in release order", () => {
    const names = ["3.9.20", "3.15.0rc2", "2.7.18", "3.15.0", "3.15.0rc10", "3.15.0b2", "3.15.0a1", "3.10.11"];
    const expected = ["3.15.0", "3.15.0rc10", "3.15.0rc2", "3.15.0b2", "3.15.0a1", "3.10.11", "3.9.20", "2.7.18"];
    expect(catalogView(names.map((name) => release(name)), defaultCatalogView()).versions.map((item) => item.version)).toEqual(expected);
    expect(compareReleaseVersions("2.0.1", "2.0")).toBeGreaterThan(0);
    expect(compareReleaseVersions("21.0.8+99999999999999999999", "21.0.8+99999999999999999998")).toBeGreaterThan(0);
    expect(isPreviewRelease(release("2.5c1"))).toBe(true);
    expect(isPreviewRelease(release("27", { channel: "early access" }))).toBe(true);
    expect(isPreviewRelease(release("22", { channel: "LTS" }))).toBe(false);
  });

  it("sorts publication dates and leaves missing or invalid dates last in either direction", () => {
    const releases = [
      release("3.10.11", { publishedAt: "2023-04-05T00:00:00Z" }),
      release("3.15.0", { publishedAt: "2026-10-07T00:00:00Z" }),
      release("2.7.18"), release("2.0", { publishedAt: "not a date" }),
    ];
    expect(catalogView(releases, { ...defaultCatalogView(), sort: "published-desc" }).versions.map((item) => item.version)).toEqual(["3.15.0", "3.10.11", "2.7.18", "2.0"]);
    expect(catalogView(releases, { ...defaultCatalogView(), sort: "published-asc" }).versions.map((item) => item.version)).toEqual(["3.10.11", "3.15.0", "2.7.18", "2.0"]);
  });

  it("does not truncate a long release history and counts filtered results separately", () => {
    const releases = Array.from({ length: 1200 }, (_, index) => release(`3.${index}.0`));
    const all = catalogView(releases, defaultCatalogView());
    expect(all.versions).toHaveLength(1200);
    expect(all.versions[0]?.version).toBe("3.1199.0");
    const filtered = catalogView(releases, { ...defaultCatalogView(), query: "3.1199.0" });
    expect([filtered.totalCount, filtered.filteredCount]).toEqual([1200, 1]);
  });

  it("only offers HTTPS official-page URLs without embedded credentials", () => {
    expect(officialReleaseUrl("https://www.python.org/downloads/release/python-2718/")).toBe("https://www.python.org/downloads/release/python-2718/");
    for (const unsafe of ["javascript:alert(1)", "data:text/html,unsafe", "http://www.python.org/", "https://user:secret@www.python.org/", "not a URL"]) {
      expect(officialReleaseUrl(unsafe)).toBeUndefined();
    }
  });
});
