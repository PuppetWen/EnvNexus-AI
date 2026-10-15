import type { VersionCatalog } from "./types";

export function latestCatalog(current: VersionCatalog | undefined, incoming: VersionCatalog): VersionCatalog {
  if (!current) return incoming;
  const currentTime = Date.parse(current.fetchedAt);
  const incomingTime = Date.parse(incoming.fetchedAt);
  if (incomingTime > currentTime || (incomingTime === currentTime && current.cached && !incoming.cached)) return incoming;
  return current;
}
