export function readMainScrollPosition(
  root: ParentNode,
  shouldPreserve: boolean,
): number | undefined {
  if (!shouldPreserve) return undefined;
  return root.querySelector<HTMLElement>(".content")?.scrollTop;
}

export function restoreMainScrollPosition(
  root: ParentNode,
  scrollTop: number | undefined,
): void {
  if (scrollTop === undefined) return;
  const content = root.querySelector<HTMLElement>(".content");
  if (content) content.scrollTop = scrollTop;
}
