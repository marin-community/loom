// Compact relative time for activity/metadata, e.g. "just now", "3m ago",
// "2h ago", "4d ago". Foundation owns this; pages import it. Takes an ISO
// timestamp (Session.last_activity_at, WeaverEvent.created_at) and an optional
// reference "now" (for tests). Returns '' for empty/invalid input.
export function timeAgo(iso: string | null | undefined, now: number = Date.now()): string {
  if (!iso) return '';
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return '';
  const secs = Math.max(0, Math.round((now - then) / 1000));
  if (secs < 45) return 'just now';
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.round(hrs / 24);
  if (days < 7) return `${days}d ago`;
  const wks = Math.round(days / 7);
  if (wks < 5) return `${wks}w ago`;
  return `${Math.round(days / 30)}mo ago`;
}

// Tighter age for compact operational metadata, e.g. "now", "10m", "1h".
// Keep this separate from timeAgo: prose/activity surfaces still benefit from
// reading "10m ago", while a PR pill should spend as little width as possible.
export function compactAge(iso: string | null | undefined, now: number = Date.now()): string {
  return timeAgo(iso, now).replace('just now', 'now').replace(' ago', '');
}

/** Locale-formatted exact time for a relative timestamp's tooltip/a11y label.
 *  Invalid input is preserved rather than silently disappearing. */
export function exactTime(iso: string): string {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleString();
}

/** Compact clock time in the viewer's browser timezone and locale. Server
 * timestamps stay absolute/UTC; only the presentation is localized here. */
export function localTime(
  iso: string | null | undefined,
  options: Intl.DateTimeFormatOptions = {},
  locales?: Intl.LocalesArgument,
): string {
  if (!iso) return '';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleTimeString(locales, {
    hour: 'numeric',
    minute: '2-digit',
    ...options,
  });
}
