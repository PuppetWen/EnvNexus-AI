import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(join(process.cwd(), "src", "app.ts"), "utf8");
const styles = readFileSync(join(process.cwd(), "src", "styles.css"), "utf8");
const themes = readFileSync(join(process.cwd(), "src", "themes.css"), "utf8");

describe("Windows path presentation", () => {
  it("disables localized yen glyph substitution for path content", () => {
    expect(themes).toContain("--font-path:");
    expect(styles).toContain('font-feature-settings: "locl" 0');
    expect(styles).toContain(".path-text");
    expect(styles).toContain(".path-input");
    expect(styles).toContain(".path-context");
  });

  it("marks the main path sources and path editors explicitly", () => {
    expect(appSource).toMatch(
      /class="path-text"[^>]*>\$\{escapeHtml\(state\.bootstrap\?\.dataRoot/,
    );
    expect(appSource).toMatch(
      /class="path-text"[^>]*title="\$\{escapeHtml\(version\.path\)/,
    );
    expect(appSource).toContain(
      '<p class="path-context">${escapeHtml(plan.summary)}</p>',
    );
    expect(appSource.match(/class="path-input"/g)?.length).toBeGreaterThanOrEqual(3);
  });

  it("does not store yen characters as path separators", () => {
    expect(appSource).not.toMatch(/[¥￥]/u);
  });
});
