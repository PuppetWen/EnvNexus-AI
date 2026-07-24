export type ViewId =
  | "dashboard"
  | "tools"
  | "tool-detail"
  | "changes"
  | "diagnostics"
  | "commands"
  | "settings";

export interface NavigationState {
  view: ViewId;
  selectedToolId?: string;
  selectedAiProviderId?: string;
}

const storageKey = "envnexus-ai.navigation";
const legacyStorageKey = "envpilot.navigation";
const validViews = new Set<ViewId>([
  "dashboard",
  "tools",
  "tool-detail",
  "changes",
  "diagnostics",
  "commands",
  "settings",
]);

export function getStoredNavigation(): NavigationState {
  try {
    const raw =
      window.localStorage.getItem(storageKey) ??
      window.localStorage.getItem(legacyStorageKey);
    if (!raw) return { view: "dashboard" };
    const parsed = JSON.parse(raw) as Partial<NavigationState> & {
      schemaVersion?: number;
    };
    if (
      parsed.schemaVersion !== 1 ||
      typeof parsed.view !== "string" ||
      !validViews.has(parsed.view as ViewId)
    ) {
      return { view: "dashboard" };
    }
    const view = parsed.view as ViewId;
    const selectedToolId =
      view === "tool-detail" &&
      typeof parsed.selectedToolId === "string" &&
      parsed.selectedToolId.trim()
        ? parsed.selectedToolId
        : undefined;
    if (view === "tool-detail" && !selectedToolId) {
      return { view: "tools" };
    }
    const navigation = {
      view,
      selectedToolId,
      selectedAiProviderId:
        typeof parsed.selectedAiProviderId === "string" &&
        parsed.selectedAiProviderId.trim()
          ? parsed.selectedAiProviderId
          : undefined,
    };
    window.localStorage.setItem(
      storageKey,
      JSON.stringify({ schemaVersion: 1, ...navigation }),
    );
    return navigation;
  } catch {
    return { view: "dashboard" };
  }
}

export function storeNavigation(navigation: NavigationState): void {
  window.localStorage.setItem(
    storageKey,
    JSON.stringify({
      schemaVersion: 1,
      view: navigation.view,
      selectedToolId:
        navigation.view === "tool-detail"
          ? navigation.selectedToolId
          : undefined,
      selectedAiProviderId: navigation.selectedAiProviderId,
    }),
  );
}
