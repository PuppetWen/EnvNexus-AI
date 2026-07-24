import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const styles = readFileSync(join(process.cwd(), "src", "styles.css"), "utf8");

describe("action button icon and label alignment", () => {
  it("uses one centered flex row and fixed-size SVG items", () => {
    expect(styles).toMatch(
      /\.scan-button,\s*\.primary-button,\s*\.secondary-button\s*\{[\s\S]*?display:\s*inline-flex;[\s\S]*?align-items:\s*center;/,
    );
    expect(styles).toMatch(
      /\.scan-button > svg,\s*\.primary-button > svg,\s*\.secondary-button > svg\s*\{[\s\S]*?display:\s*block;[\s\S]*?flex:\s*0 0 auto;/,
    );
  });
});
