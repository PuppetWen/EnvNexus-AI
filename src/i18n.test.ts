import { describe, expect, it } from "vitest";
import { localizeUi } from "./i18n";

describe("interface localization", () => {
  it("translates navigation and application settings without changing data attributes", () => {
    const root = document.createElement("div");
    root.innerHTML = `
      <button data-nav="settings"><span>设置</span></button>
      <label><span>点击窗口关闭按钮时</span></label>
      <button id="save-app-preferences">保存设置</button>
    `;

    localizeUi(root, "en-US");

    expect(document.documentElement.lang).toBe("en-US");
    expect(root.querySelector("[data-nav='settings']")?.textContent).toBe("Settings");
    expect(root.textContent).toContain("When the close button is clicked");
    expect(root.querySelector("#save-app-preferences")?.textContent).toBe("Save settings");
    expect(root.querySelector("[data-nav='settings']")?.getAttribute("data-nav")).toBe(
      "settings",
    );
  });
});
