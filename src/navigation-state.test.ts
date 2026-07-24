import { beforeEach, describe, expect, it } from "vitest";
import { getStoredNavigation, storeNavigation } from "./navigation-state";

describe("last navigation persistence", () => {
  beforeEach(() => window.localStorage.clear());

  it("restores a tool detail page and selected AI provider", () => {
    storeNavigation({
      view: "tool-detail",
      selectedToolId: "java",
      selectedAiProviderId: "deepseek",
    });
    expect(getStoredNavigation()).toEqual({
      view: "tool-detail",
      selectedToolId: "java",
      selectedAiProviderId: "deepseek",
    });
  });

  it("falls back safely for invalid or incomplete stored pages", () => {
    window.localStorage.setItem(
      "envnexus-ai.navigation",
      JSON.stringify({ schemaVersion: 1, view: "removed-page" }),
    );
    expect(getStoredNavigation()).toEqual({ view: "dashboard" });

    window.localStorage.setItem(
      "envnexus-ai.navigation",
      JSON.stringify({ schemaVersion: 1, view: "tool-detail" }),
    );
    expect(getStoredNavigation()).toEqual({ view: "tools" });
  });

  it("migrates the legacy EnvPilot storage key", () => {
    window.localStorage.setItem(
      "envpilot.navigation",
      JSON.stringify({ schemaVersion: 1, view: "diagnostics" }),
    );
    expect(getStoredNavigation()).toEqual({ view: "diagnostics" });
    expect(window.localStorage.getItem("envnexus-ai.navigation")).toContain(
      "diagnostics",
    );
  });
});
