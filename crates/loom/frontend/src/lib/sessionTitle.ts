export interface SessionTitlePart {
  text: string;
  muted: boolean;
}

const PATH_SEPARATOR = /\s+\/\s+/;

export function sessionPathSegments(title: string): string[] {
  return title.split(PATH_SEPARATOR).filter((part) => part.length > 0);
}

// A ` / ` inside a task title is literal text. Nested rows mute only the
// leading segments that repeat the parent row's path; smart views mute the
// group qualifier in front of that title.
export function sessionTitleParts(
  title: string,
  options: { group?: string | null; parentTitle?: string | null } = {},
): SessionTitlePart[] {
  const parts: SessionTitlePart[] = [];
  const group = options.group?.trim();
  if (group) parts.push({ text: group, muted: true });

  const mutedCount = sharedAncestorSegments(title, options.parentTitle);
  if (mutedCount === 0) {
    parts.push({ text: title, muted: false });
    return parts;
  }

  const segments = sessionPathSegments(title);
  for (let index = 0; index < mutedCount; index += 1) {
    parts.push({ text: segments[index], muted: true });
  }
  const rest = segments.slice(mutedCount).join(' / ');
  if (rest) parts.push({ text: rest, muted: false });
  return parts;
}

function sharedAncestorSegments(title: string, parentTitle: string | null | undefined): number {
  if (!parentTitle) return 0;
  const path = sessionPathSegments(title);
  const parent = sessionPathSegments(parentTitle);
  let count = 0;
  while (
    count < parent.length &&
    count < path.length - 1 &&
    parent[count].localeCompare(path[count], undefined, { sensitivity: 'base' }) === 0
  ) {
    count += 1;
  }
  return count;
}
