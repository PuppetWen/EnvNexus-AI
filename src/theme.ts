import type { ThemeId } from "./types";

export const themes: Array<{ id: ThemeId; label: string; hint: string }> = [
  { id: "modern-tech", label: "现代科技", hint: "薄荷青 HUD 与数据仪表" },
  { id: "cyberpunk", label: "赛博朋克", hint: "霓虹紫、扫描线与锐角" },
  { id: "japanese-light", label: "日系轻量", hint: "雾白与朱砂红" },
  { id: "game-hud", label: "游戏 HUD", hint: "黑橙霓虹、蜂窝与六边形战术框" },
  { id: "professional-minimal", label: "专业极简", hint: "中性灰与克制蓝" },
];

const storageKey = "envnexus-ai.theme";
const legacyStorageKey = "envpilot.theme";
const lightThemes = new Set<ThemeId>(["japanese-light", "professional-minimal"]);

export function getStoredTheme(): ThemeId {
  const value =
    window.localStorage.getItem(storageKey) ??
    window.localStorage.getItem(legacyStorageKey);
  if (themes.some((theme) => theme.id === value)) {
    window.localStorage.setItem(storageKey, value as ThemeId);
    return value as ThemeId;
  }
  return "modern-tech";
}

export function applyTheme(theme: ThemeId): void {
  document.documentElement.dataset.theme = theme;
  window.localStorage.setItem(storageKey, theme);
  if ("__TAURI_INTERNALS__" in window) {
    void import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) =>
        getCurrentWindow().setTheme(lightThemes.has(theme) ? "light" : "dark"),
      )
      .catch(() => {
        // CSS theme application must not fail if native chrome synchronization is unavailable.
      });
  }
}
