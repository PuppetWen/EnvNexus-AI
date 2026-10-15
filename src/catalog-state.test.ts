import { describe, expect, it } from "vitest";
import { latestCatalog } from "./catalog-state";
import type { VersionCatalog } from "./types";

function catalog(fetchedAt: string, cached: boolean): VersionCatalog {
  return { toolId: "node", sourceName: "Node.js", sourceUrl: "https://nodejs.org/dist/index.json", fetchedAt, cached, versions: [] };
}

describe("catalog results arriving out of order", () => {
  it("accepts a background refresh before the initial cache response", () => {
    const fresh = catalog("2026-10-14T14:01:00Z", false);
    const old = catalog("2026-10-14T14:00:00Z", true);
    expect(latestCatalog(latestCatalog(undefined, fresh), old)).toBe(fresh);
  });

  it("prefers a live result when timestamps are identical", () => {
    const fresh = catalog("2026-10-14T14:00:00Z", false);
    const old = catalog(fresh.fetchedAt, true);
    expect(latestCatalog(old, fresh)).toBe(fresh);
    expect(latestCatalog(fresh, old)).toBe(fresh);
  });
});
