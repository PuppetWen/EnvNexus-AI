import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(join(process.cwd(), "src", "app.ts"), "utf8");
const styles = readFileSync(join(process.cwd(), "src", "styles.css"), "utf8");

describe("background download and installation UI", () => {
  it("moves a confirmed operation out of the blocking modal", () => {
    expect(appSource).toContain("function renderBackgroundOperation()");
    expect(appSource).toContain("state.operationPanelMinimized = true;");
    expect(appSource).toContain('id="show-operation-progress"');
    expect(appSource).toContain('id="minimize-operation"');
  });

  it("updates operation progress without rebuilding the application shell", () => {
    const listenerStart = appSource.indexOf(
      'await listen<OperationProgress>("operation-progress"',
    );
    const listenerEnd = appSource.indexOf(
      'await listen<TrayAction>("tray-action"',
      listenerStart,
    );
    const listener = appSource.slice(listenerStart, listenerEnd);

    expect(listener).toContain("updateOperationProgressUi()");
    expect(listener).not.toContain("render()");
  });

  it("updates application-update progress in place", () => {
    const callbackStart = appSource.indexOf("await update.downloadAndInstall");
    const callbackEnd = appSource.indexOf("await relaunch()", callbackStart);
    const callback = appSource.slice(callbackStart, callbackEnd);

    expect(callback).toContain("updateApplicationUpdateProgressUi()");
    expect(callback).not.toContain("render()");
  });

  it("uses an adaptive dock row that does not cover page content", () => {
    expect(appSource).toContain('" has-background-operation"');
    expect(styles).toContain(".main.has-background-operation");
    expect(styles).toContain(
      "grid-template-rows: 68px minmax(0, 1fr) auto",
    );
    expect(styles).toContain("width: min(560px, calc(100% - 48px))");
    expect(styles).toContain("justify-self: end");
    expect(styles).not.toContain("position: fixed;\n  z-index: 12");
  });
});
