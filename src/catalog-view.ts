import type { RemoteVersion } from "./types";

export type CatalogChannel = "all" | "stable" | "preview" | "archive";
export type CatalogSort = "version-desc" | "version-asc" | "published-desc" | "published-asc";

export interface CatalogViewPreferences {
  query: string;
  channel: CatalogChannel;
  installableOnly: boolean;
  sort: CatalogSort;
}

export function defaultCatalogView(): CatalogViewPreferences {
  return { query: "", channel: "all", installableOnly: false, sort: "version-desc" };
}

export function installUnavailableReason(version: RemoteVersion): string | undefined {
  if (version.installUnavailableReason != null) {
    return version.installUnavailableReason.trim() || "当前发行包尚不支持自动安装";
  }
  return version.downloadUrl?.trim() ? undefined : "无 Windows 安装包";
}

export function isPreviewRelease(version: RemoteVersion): boolean {
  const channel = /(?:^|[\s._-])(?:a|alpha|b|beta|c|rc|pre|preview|prerelease|pre-release|dev|development|snapshot|nightly|early.?access|ea|candidate)(?:\d|[\s._-]|$)/i;
  const name = /(?:\d|[\s._+-])(?:a|alpha|b|beta|c|rc|pre|preview|dev|snapshot|nightly|ea|candidate)(?:\d|[\s._+-]|$)/i;
  return channel.test(version.channel) || name.test(version.version);
}

function releaseChannel(version: RemoteVersion): Exclude<CatalogChannel, "all"> {
  if (version.channel.trim().toLowerCase() === "archive") return "archive";
  return isPreviewRelease(version) ? "preview" : "stable";
}

function qualifierRank(value: string): number {
  if (["dev", "snapshot", "nightly"].includes(value)) return 0;
  if (["a", "alpha"].includes(value)) return 1;
  if (["b", "beta"].includes(value)) return 2;
  if (["c", "rc", "candidate"].includes(value)) return 4;
  if (["stable", "final", "release"].includes(value)) return 5;
  return 3;
}

function compareNumbers(left: string, right: string): number {
  left = left.replace(/^0+/, "") || "0";
  right = right.replace(/^0+/, "") || "0";
  return left.length - right.length || (left < right ? -1 : left > right ? 1 : 0);
}

function compareEndedVersion(remaining: string[]): number {
  for (const token of remaining) {
    if (/^\d+$/.test(token)) {
      if (compareNumbers(token, "0") !== 0) return 1;
    } else if (!["stable", "final", "release"].includes(token)) {
      return -1;
    }
  }
  return 0;
}

export function compareReleaseVersions(left: string, right: string): number {
  const tokens = (value: string): string[] => {
    const start = value.search(/\d/);
    return (start < 0 ? value : value.slice(start)).toLowerCase().match(/\d+|[a-z]+/g) ?? [];
  };
  const leftTokens = tokens(left);
  const rightTokens = tokens(right);
  const shared = Math.min(leftTokens.length, rightTokens.length);
  for (let index = 0; index < shared; index += 1) {
    const a = leftTokens[index]!;
    const b = rightTokens[index]!;
    const aNumber = /^\d+$/.test(a);
    const bNumber = /^\d+$/.test(b);
    const result = aNumber && bNumber ? compareNumbers(a, b)
      : aNumber !== bNumber ? (aNumber ? 1 : -1)
        : qualifierRank(a) - qualifierRank(b) || a.localeCompare(b, "en");
    if (result !== 0) return result;
  }
  if (leftTokens.length !== rightTokens.length) {
    return leftTokens.length > shared
      ? compareEndedVersion(leftTokens.slice(shared))
      : -compareEndedVersion(rightTokens.slice(shared));
  }
  return left.localeCompare(right, "en", { sensitivity: "base" });
}

function publicationTime(version: RemoteVersion): number | undefined {
  const time = version.publishedAt ? Date.parse(version.publishedAt) : NaN;
  return Number.isFinite(time) ? time : undefined;
}

export function catalogView(versions: readonly RemoteVersion[], preferences: CatalogViewPreferences) {
  const terms = preferences.query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  const filtered = versions.filter((version) => {
    if (preferences.installableOnly && installUnavailableReason(version)) return false;
    if (preferences.channel !== "all" && releaseChannel(version) !== preferences.channel) return false;
    const text = [version.version, version.channel, version.architecture, version.publishedAt, installUnavailableReason(version)].join(" ").toLocaleLowerCase();
    return terms.every((term) => text.includes(term));
  });
  filtered.sort((left, right) => {
    if (preferences.sort.startsWith("published-")) {
      const a = publicationTime(left);
      const b = publicationTime(right);
      if (a === undefined && b !== undefined) return 1;
      if (a !== undefined && b === undefined) return -1;
      if (a !== undefined && b !== undefined && a !== b) {
        return preferences.sort === "published-asc" ? a - b : b - a;
      }
    }
    const versionOrder = compareReleaseVersions(left.version, right.version);
    return preferences.sort === "version-asc" ? versionOrder : -versionOrder;
  });
  return {
    versions: filtered,
    totalCount: versions.length,
    filteredCount: filtered.length,
    installableCount: versions.filter((version) => !installUnavailableReason(version)).length,
  };
}

export function officialReleaseUrl(value: string | null | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password ? url.href : undefined;
  } catch {
    return undefined;
  }
}
