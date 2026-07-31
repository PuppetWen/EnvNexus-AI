import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(join(process.cwd(), "src", "app.ts"), "utf8");
const styles = readFileSync(join(process.cwd(), "src", "styles.css"), "utf8");
const versioningSource = readFileSync(
  join(process.cwd(), "src-tauri", "src", "versioning.rs"),
  "utf8",
);

describe("official version catalog queries", () => {
  it("tracks loading state independently from transient DOM nodes", () => {
    expect(appSource).toContain("fetchingCatalogs: Set<string>");
    expect(appSource).toContain("state.fetchingCatalogs.add(toolId)");
    expect(appSource).toContain("state.fetchingCatalogs.delete(toolId)");
    expect(appSource).toContain("finally {");
  });

  it("shows a cached result instead of leaving the query spinner active", () => {
    expect(appSource).toContain("catalog.cached");
    expect(appSource).toContain("网络不可用，显示上次成功结果");
    expect(appSource).toContain('fetchingCatalog ? "disabled" : ""');
  });

  it("hides redundant English decorative labels in Chinese UI", () => {
    expect(styles).toContain(":root:lang(zh-CN) .eyebrow");
    expect(styles).toContain(":root:lang(zh-TW) .eyebrow");
  });

  it("sorts queried releases by numeric version in descending order", () => {
    expect(versioningSource).toContain("sort_remote_versions_descending");
    expect(versioningSource).toContain(
      'sorted(&["3.13.14", "3.14.6", "3.9.20", "3.14.5"])',
    );
    expect(versioningSource).toContain(
      '["3.14.6", "3.14.5", "3.13.14", "3.9.20"]',
    );
  });
});
