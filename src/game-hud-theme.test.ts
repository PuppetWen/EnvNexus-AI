import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const themeCss = readFileSync(join(process.cwd(), "src", "themes.css"), "utf8");

describe("game HUD visual language", () => {
  it("uses the black-orange honeycomb palette from the game HUD reference", () => {
    expect(themeCss).toContain(':root[data-theme="game-hud"]');
    expect(themeCss).toContain("--app-bg: #080706");
    expect(themeCss).toContain("--accent: #ff6a1f");
    expect(themeCss).toContain("data:image/svg+xml");
    expect(themeCss).toContain("stroke='%23ff6a1f'");
    expect(themeCss).toContain("90px 52px");
  });

  it("scopes hexagonal controls and neon frames to game HUD only", () => {
    expect(themeCss).toContain(
      ':root[data-theme="game-hud"] .ai-provider-brand',
    );
    expect(themeCss).toContain(
      "clip-path: polygon(25% 0, 75% 0, 100% 50%",
    );
    expect(themeCss).toContain(
      ':root[data-theme="game-hud"] .score-ring',
    );
    expect(themeCss).toContain("0 0 28px rgba(255, 67, 0, 0.28)");
  });
});
