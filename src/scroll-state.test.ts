import { beforeEach, describe, expect, it } from "vitest";
import {
  readMainScrollPosition,
  restoreMainScrollPosition,
} from "./scroll-state";

describe("main content scroll preservation", () => {
  beforeEach(() => {
    document.body.innerHTML = '<main id="root"><section class="content"></section></main>';
  });

  it("restores the previous offset when the current page re-renders", () => {
    const root = document.querySelector<HTMLElement>("#root")!;
    root.querySelector<HTMLElement>(".content")!.scrollTop = 684;
    const saved = readMainScrollPosition(root, true);

    root.innerHTML = '<section class="content"></section>';
    restoreMainScrollPosition(root, saved);

    expect(root.querySelector<HTMLElement>(".content")!.scrollTop).toBe(684);
  });

  it("does not carry an offset into a different page", () => {
    const root = document.querySelector<HTMLElement>("#root")!;
    root.querySelector<HTMLElement>(".content")!.scrollTop = 684;
    expect(readMainScrollPosition(root, false)).toBeUndefined();
  });
});
