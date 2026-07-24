import { beforeEach, describe, expect, it } from "vitest";
import { applyTheme, getStoredTheme } from "./theme";

describe("theme persistence", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
  });

  it("uses modern tech by default", () => {
    expect(getStoredTheme()).toBe("modern-tech");
  });

  it("persists and applies a valid theme", () => {
    applyTheme("japanese-light");
    expect(getStoredTheme()).toBe("japanese-light");
    expect(document.documentElement.dataset.theme).toBe("japanese-light");
  });

  it("ignores an invalid stored value", () => {
    window.localStorage.setItem("envnexus-ai.theme", "unknown");
    expect(getStoredTheme()).toBe("modern-tech");
  });

  it("migrates a valid legacy EnvPilot theme", () => {
    window.localStorage.setItem("envpilot.theme", "game-hud");
    expect(getStoredTheme()).toBe("game-hud");
    expect(window.localStorage.getItem("envnexus-ai.theme")).toBe("game-hud");
  });
});
