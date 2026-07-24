import { describe, expect, it } from "vitest";
import { hasToolBrandIcon, toolBrandIcon } from "./brand-icons";

const toolIds = [
  "python",
  "java",
  "go",
  "rust",
  "node",
  "git",
  "maven",
  "dotnet",
  "ruby",
  "php",
  "android-sdk",
  "android-ndk",
  "gradle",
  "cmake",
  "adb",
];

describe("tool brand icons", () => {
  it("covers every built-in tool with a vector brand glyph", () => {
    for (const toolId of toolIds) {
      expect(hasToolBrandIcon(toolId)).toBe(true);
      expect(toolBrandIcon(toolId)).toContain("<svg");
      expect(toolBrandIcon(toolId)).toMatch(/brand-icon-(main|svg-original)/);
    }
  });

  it("uses the Java coffee glyph instead of a Java letter badge", () => {
    const java = toolBrandIcon("java");
    expect(java).toContain('aria-label="Java"');
    expect(java).toContain("#EA2D2E");
    expect(java).toContain("#0074BD");
    expect(java).not.toContain(">Jv<");
  });
});
